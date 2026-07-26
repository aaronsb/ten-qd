//! The cassette adapter.
//!
//! The tape-shaped thing with a headphone wire hanging off it, that you
//! plugged into a Discman and pushed into the car's deck. The mechanism spun
//! and believed it was playing a tape; the audio actually arrived over the
//! cable and then went through everything downstream — the deck's playback
//! amp, the equaliser, the fader, the power amp, the speakers.
//!
//! That is exactly what this does. A PipeWire null sink named
//! `ten-qd cassette adapter` is the wire: anything that can choose an output
//! device can plug into it, and what comes out the other side has been through
//! the whole rack. It is the same trick EasyEffects uses for its virtual sink,
//! which is worth saying plainly — the pattern is well-trodden, and the only
//! thing invented here is what it is called.
//!
//! A side effect worth knowing about: the sink is a real system audio device,
//! so *anything* can use it. Discord will offer it as an output. That is not
//! an accident of the design, it is the design.
//!
//! Everything here shells out to `pactl`, which speaks to both PipeWire and
//! PulseAudio and is present on any desktop that has either. No library, no
//! D-Bus, no protocol of our own.

use std::process::Command;

use anyhow::{bail, Context, Result};

/// The sink's internal name; the monitor is this plus `.monitor`.
pub const SINK: &str = "ten_qd_adapter";
pub const MONITOR: &str = "ten_qd_adapter.monitor";
/// What the rest of the desktop calls it in device pickers.
pub const DESCRIPTION: &str = "ten-qd cassette adapter";

/// A playback stream on the system — a candidate to plug into the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub index: u32,
    /// The application, e.g. `Chromium`, `spotify`, `mpv`.
    pub app: String,
    /// What it is playing, when it says.
    pub media: String,
    /// The sink it is currently on.
    pub sink: u32,
}

impl Stream {
    pub fn label(&self) -> String {
        if self.media.is_empty() || self.media == self.app {
            self.app.clone()
        } else {
            format!("{} — {}", self.app, self.media)
        }
    }
}

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl")
        .args(args)
        .output()
        .context("pactl not found — the adapter needs PipeWire or PulseAudio")?;
    if !out.status.success() {
        bail!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every playback stream currently on the system, whichever sink it is on.
///
/// Listing does not require the adapter to exist — you want to see what is
/// playing *before* deciding to plug it in.
pub fn streams() -> Result<Vec<Stream>> {
    // Two names to exclude, not one. The running executable's name covers a
    // renamed or wrapped binary; the fixed package name covers the case the
    // first version got wrong — under `cargo test` the executable is
    // `ten_qd-<hash>`, so an exe-name match alone let a real ten-qd's output
    // stream through and offered a feedback loop as a candidate.
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let pkg = env!("CARGO_PKG_NAME");
    let json = pactl(&["-f", "json", "list", "sink-inputs"])?;
    let v: serde_json::Value = serde_json::from_str(&json).context("pactl gave unparseable json")?;
    let Some(items) = v.as_array() else { return Ok(Vec::new()) };

    Ok(items
        .iter()
        .filter_map(|s| {
            let props = s.get("properties")?;
            let get = |k: &str| props.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

            // Our own output stream must never be a candidate: plugging the
            // rack's output back into its own input is a feedback loop, and
            // it is the one stream in the list that can create one.
            //
            // Identifying it is fiddlier than it should be. PipeWire does not
            // populate `application.process.id` here at all, so a PID test
            // matches nothing and silently lets the loop close. What it does
            // set is `node.name = alsa_playback.ten-qd` and
            // `application.name = PipeWire ALSA [ten-qd]` — both carry the
            // executable name, so that is what is matched.
            let identity = format!("{} {}", get("node.name"), get("application.name"));
            if identity.contains(pkg) || (!exe.is_empty() && identity.contains(&exe)) {
                return None;
            }

            let app = match get("application.name") {
                a if !a.is_empty() => a,
                _ => get("application.process.binary"),
            };
            Some(Stream {
                index: s.get("index")?.as_u64()? as u32,
                sink: s.get("sink").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                media: get("media.name"),
                app: if app.is_empty() { "unknown".into() } else { app },
            })
        })
        .collect())
}

/// A loaded null sink, and a memory of every stream moved onto it.
pub struct Adapter {
    module: u32,
    /// `(sink-input, the sink it came from)` — so eject can put things back
    /// where it found them rather than dumping everything on the default.
    moved: Vec<(u32, u32)>,
}

impl Adapter {
    /// Insert the adapter: create the sink.
    ///
    /// If one is already loaded — a previous run that did not get to clean up
    /// — it is adopted rather than duplicated.
    pub fn insert() -> Result<Self> {
        if let Some(module) = existing_module()? {
            return Ok(Adapter { module, moved: Vec::new() });
        }
        let out = pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={SINK}"),
            &format!("sink_properties=device.description=\"{DESCRIPTION}\""),
        ])?;
        let module = out
            .trim()
            .parse()
            .with_context(|| format!("pactl returned {out:?} instead of a module index"))?;
        Ok(Adapter { module, moved: Vec::new() })
    }

    /// Route a stream through the rack.
    pub fn plug(&mut self, s: &Stream) -> Result<()> {
        pactl(&["move-sink-input", &s.index.to_string(), SINK])?;
        if !self.moved.iter().any(|(i, _)| *i == s.index) {
            self.moved.push((s.index, s.sink));
        }
        Ok(())
    }

    /// Put every moved stream back on the sink it came from.
    ///
    /// Failures are ignored on purpose: a stream that ended while plugged in
    /// no longer exists, and that is the ordinary case, not an error.
    pub fn unplug_all(&mut self) {
        for (input, sink) in self.moved.drain(..) {
            let _ = pactl(&["move-sink-input", &input.to_string(), &sink.to_string()]);
        }
    }

    /// Eject: restore every stream, then remove the sink. Called on drop too,
    /// because leaving a phantom audio device behind after the program exits
    /// would be rude to the whole desktop.
    pub fn eject(&mut self) {
        self.unplug_all();
        let _ = pactl(&["unload-module", &self.module.to_string()]);
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.eject();
    }
}

/// Find an adapter sink left over from a previous run.
fn existing_module() -> Result<Option<u32>> {
    let Ok(json) = pactl(&["-f", "json", "list", "modules"]) else { return Ok(None) };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else { return Ok(None) };
    let Some(items) = v.as_array() else { return Ok(None) };

    Ok(items.iter().find_map(|m| {
        let args = m.get("argument")?.as_str()?;
        args.contains(&format!("sink_name={SINK}"))
            .then(|| m.get("index")?.as_u64().map(|i| i as u32))
            .flatten()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_labels_read_as_app_and_title() {
        let s = Stream {
            index: 1,
            app: "Chromium".into(),
            media: "YouTube Music".into(),
            sink: 0,
        };
        assert_eq!(s.label(), "Chromium — YouTube Music");
    }

    #[test]
    fn a_stream_that_only_names_itself_is_not_repeated() {
        let s = Stream { index: 1, app: "mpv".into(), media: "mpv".into(), sink: 0 };
        assert_eq!(s.label(), "mpv");
        let s = Stream { index: 1, app: "mpv".into(), media: String::new(), sink: 0 };
        assert_eq!(s.label(), "mpv");
    }

    /// The self-exclusion is the one filter that must not be got wrong, so it
    /// is asserted against the live system: whatever else is playing, this
    /// process's own output stream is never offered as something to plug in.
    #[test]
    fn our_own_output_is_never_a_candidate() {
        let Ok(list) = streams() else { return };
        for s in &list {
            assert!(!s.app.contains("ten-qd"), "offered our own stream: {s:?}");
        }
    }

    /// Exercises the real `pactl` when one is present. Listing is read-only,
    /// so this is safe to run anywhere; on a machine without PipeWire or
    /// PulseAudio it simply reports the absence rather than failing.
    #[test]
    fn listing_streams_either_works_or_says_why() {
        match streams() {
            Ok(list) => {
                for s in &list {
                    assert!(!s.app.is_empty(), "every stream must name something");
                }
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("pactl") || msg.contains("json"),
                    "an unhelpful failure: {msg}"
                );
            }
        }
    }
}
