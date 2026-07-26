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

/// An output the rack can drive: PipeWire's own name for it, and the friendly
/// description the desktop shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sink {
    pub name: String,
    pub description: String,
}

/// Every output sink, as the desktop sees them.
///
/// This replaced enumerating cpal/ALSA devices, which listed ALSA's plugin
/// chain and hardware nodes under names nobody recognises — and, worse, left
/// out Bluetooth entirely, because ALSA does not enumerate bluez sinks. The
/// user's actual default output was missing from the picker.
pub fn sinks() -> Vec<Sink> {
    let Ok(json) = pactl(&["-f", "json", "list", "sinks"]) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else { return Vec::new() };
    let Some(items) = v.as_array() else { return Vec::new() };

    items
        .iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?.to_string();
            // Never offer our own adapter as an output: that is the loop.
            if name == SINK {
                return None;
            }
            let description = s
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or(&name)
                .to_string();
            Some(Sink { name, description })
        })
        .collect()
}

/// Send this process's own audio to `sink`.
///
/// Moving our own sink-input is how the rack holds an output independent of
/// the system default, and it takes effect immediately — no stream rebuild,
/// no restart. It is the same `move-sink-input` used to plug a stream into
/// the adapter, pointed the other way.
pub fn route_own_output(sink: &str) -> Result<()> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let pkg = env!("CARGO_PKG_NAME");

    let json = pactl(&["-f", "json", "list", "sink-inputs"])?;
    let v: serde_json::Value = serde_json::from_str(&json)?;
    let Some(items) = v.as_array() else { bail!("no streams") };

    let mine = items.iter().find_map(|s| {
        let props = s.get("properties")?;
        let get = |k: &str| props.get(k).and_then(|x| x.as_str()).unwrap_or("");
        let identity = format!("{} {}", get("node.name"), get("application.name"));
        (identity.contains(pkg) || (!exe.is_empty() && identity.contains(&exe)))
            .then(|| s.get("index")?.as_u64())
            .flatten()
    });

    let Some(index) = mine else { bail!("our own output stream is not on the graph yet") };
    pactl(&["move-sink-input", &index.to_string(), sink])?;
    Ok(())
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
    pub fn insert() -> Result<Self> {
        // Never adopt a leftover. A sink from a dead process has no capture
        // attached, so anything still routed to it is playing into nothing.
        remove_orphan();
        let out = pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={SINK}"),
            // `priority.session=0` matters more than it looks. WirePlumber
            // elects a default output by session priority, and a freshly
            // created sink can win that election — which silently moves *the
            // whole desktop's* audio into the adapter. If ten-qd then exits
            // without cleaning up, sound goes nowhere and the cause is not
            // remotely obvious. A virtual device should never be a candidate
            // for the default output.
            &format!(
                "sink_properties=device.description=\"{DESCRIPTION}\" priority.session=0 priority.driver=0"
            ),
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

/// Remove an adapter sink left behind by a previous run.
///
/// Called at start-up and before inserting. If ten-qd is killed rather than
/// asked to quit, `Drop` never runs and the sink outlives the process —
/// with whatever was plugged into it still routed there, now playing into
/// nothing. Unloading the module makes PipeWire move those streams back to
/// the default output, which is the outcome the user wants and cannot easily
/// arrive at themselves, because the cause is invisible.
pub fn remove_orphan() {
    if let Ok(Some(module)) = existing_module() {
        let _ = pactl(&["unload-module", &module.to_string()]);
    }
}

/// Find an adapter sink left over from a previous run.
///
/// Uses `pactl list short modules` rather than the JSON form, because the
/// JSON for modules carries no `index` field at all — only `argument`,
/// `name`, `properties` and `usage_counter`. The lookup this replaced asked
/// for `index`, got nothing every time, and so silently never found an
/// orphan. The short form is tab-separated `index<TAB>name<TAB>argument`.
fn existing_module() -> Result<Option<u32>> {
    let Ok(text) = pactl(&["list", "short", "modules"]) else { return Ok(None) };
    Ok(text.lines().find_map(|line| {
        let mut f = line.split('\t');
        let index: u32 = f.next()?.trim().parse().ok()?;
        let name = f.next()?;
        let args = f.next().unwrap_or("");
        (name == "module-null-sink" && args.contains(&format!("sink_name={SINK}"))).then_some(index)
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
