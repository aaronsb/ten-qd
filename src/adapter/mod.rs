//! The auxiliary input — which is to say, the wire.
//!
//! This began as a cassette adapter: the tape-shaped shell with a headphone
//! lead, pushed into the deck so the mechanism would spin and believe it was
//! playing. A lovely object, and the wrong interface — a deck carrying one has
//! a counter that counts nothing and two sides that do not exist. The cable is
//! a *source*, so now it is one, and the deck went back to being a deck.
//!
//! The mechanism is unchanged. A PipeWire null sink named `ten-qd aux input`
//! is the wire: anything that can choose an output device can plug into it,
//! and what comes out the other side has been through the whole rack. It is the same trick EasyEffects uses for its virtual sink,
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
//!
//! What the wire *is* lives here. Whether the signal actually went down it is
//! [`route`] — a different question, and one that turned out to need a whole
//! vocabulary of its own, because every routing this program asks for is a
//! claim that stops being true the moment some other program decides otherwise.

pub mod route;

pub use route::{
    own_output_is_looping, reseat, route_own_output, routing, safe_output, Act, Guard, Link,
    Routing,
};

use std::process::Command;

use anyhow::{bail, Context, Result};

/// The sink's internal name.
///
/// `pactl` will also list a source called `ten_qd_aux.monitor`, but that
/// name exists only on the PulseAudio side — there is no PipeWire node by it,
/// and asking `pw-record` to target it silently records a microphone instead.
/// Capture targets this name and taps the monitor ports; see
/// [`crate::audio::capture`].
pub const SINK: &str = "ten_qd_aux";
/// What the rest of the desktop calls it in device pickers.
pub const DESCRIPTION: &str = "ten-qd aux input";

/// A playback stream on the system — a candidate to plug into the aux input.
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

/// What application a stream belongs to.
///
/// One spelling, because the picker and the successor search have to agree
/// about it — the picker names the application the operator chose, and the
/// search looks for the stream that replaced it. Two spellings of "which app is
/// this" would mean adopting a stream the operator never picked, or failing to
/// adopt the one they did.
fn app_of(props: &serde_json::Value) -> String {
    let get = |k: &str| props.get(k).and_then(|x| x.as_str()).unwrap_or("");
    match get("application.name") {
        a if !a.is_empty() => a.to_string(),
        _ => get("application.process.binary").to_string(),
    }
}

/// The plug the guard is watching: a sink-input index, and whose it is.
///
/// The index alone is not enough to watch anything for long. Applications tear
/// down and rebuild their playback stream constantly — Chrome does it every few
/// minutes, a track change is enough for some players — and each rebuild is a
/// new index. Watching only the number means the plug is silently lost the
/// first time that happens, and the bay goes on describing a signal path
/// belonging to a stream that no longer exists.
///
/// So the *application* is the thing the operator actually chose, and the index
/// is only where it happens to live right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plug {
    pub index: u32,
    pub app: String,
}

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl")
        .args(args)
        .output()
        .context("pactl not found — the aux input needs PipeWire or PulseAudio")?;
    if !out.status.success() {
        bail!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every playback stream currently on the system, whichever sink it is on.
///
/// Listing does not require the sink to exist — you want to see what is
/// playing *before* deciding to plug it in.
pub fn streams() -> Result<Vec<Stream>> {
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
            if is_ours(props) {
                return None;
            }

            let app = app_of(props);
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
            // Never offer our own input as an output: that is the loop.
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

/// Is this sink-input one of ours?
///
/// Three call sites grew their own copy of this test, which is three chances
/// for them to disagree about what "ours" means — and the thing two of them
/// guard is a feedback loop. PipeWire does not populate
/// `application.process.id` for sink-inputs at all, so a PID test matches
/// nothing and silently lets the loop close. What it does set is
/// `node.name = alsa_playback.ten-qd` and
/// `application.name = PipeWire ALSA [ten-qd]`, both of which carry the
/// executable name. The package name is checked as well, because under
/// `cargo test` the executable is `ten_qd-<hash>` and an exe-name match alone
/// let a real ten-qd's output stream through.
fn is_ours(props: &serde_json::Value) -> bool {
    // Resolved once. This is a per-stream predicate now that it is shared, and
    // it runs over every stream on the system four times a second; the two
    // copies it replaced each read `/proc/self/exe` once per call.
    static EXE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let exe = EXE.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default()
    });

    let get = |k: &str| props.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let identity = format!("{} {}", get("node.name"), get("application.name"));
    identity.contains(env!("CARGO_PKG_NAME")) || (!exe.is_empty() && identity.contains(exe))
}

/// Whether the adapter is on its way out, and streams must stop being moved.
///
/// Set as eject begins and cleared as the adapter goes back in, because the
/// bay's POWER key can take it out and put it back all evening.
///
/// Ejecting is two `pactl` calls with a gap between them — put every borrowed
/// stream back, then unload the sink. A re-seat landing in that gap would move
/// a stream we had just restored back onto a sink that is about to vanish, and
/// the unload would then tip it onto the system default instead of the sink it
/// came from, quietly breaking the one promise eject makes.
///
/// This *narrows* that window rather than closing it: the guard can read this
/// flag and still be beaten to the fork. But it goes from two `pactl`
/// round-trips to the gap between an atomic load and an `exec` — some four
/// orders of magnitude — for a consequence of one stream landing on the default
/// instead of where it came from, once, at shutdown. Closing it properly wants
/// a mutex held across both of eject's calls, which is a lot of machinery for
/// that.
static QUIESCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
        // The sink is in; moving streams onto it is meaningful again.
        QUIESCED.store(false, std::sync::atomic::Ordering::Release);
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
        // Before the first of the two calls, so nothing can re-seat a stream
        // into the gap between putting it back and unloading the sink.
        QUIESCED.store(true, std::sync::atomic::Ordering::Release);
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
    /// The rack's own input must never be offered as its output. That is the
    /// one choice in the picker that makes a feedback loop, and the guard is
    /// what keeps the watchdog's fallback safe by construction.
    /// Whatever it settles on, it is never the loop.
    /// The graph as it actually stood on the machine where this was found:
    /// EasyEffects holding every stream, including ours, while the panel
    /// claimed the Bluetooth headphones. Indices are the real ones.
    /// The whole point. `move-sink-input` was granted and then undone, and the
    /// panel has to be able to say so — and say by whom, because "it did not
    /// work" sends you hunting through your own gain structure.
    /// A stream that ended is not a fault, and must not light a warning: that
    /// is what stopping the music looks like from here.
    /// A plug whose application has stopped playing altogether — no stream at
    /// the tracked index, and none belonging to it anywhere. That is what
    /// stopping the music looks like from here, and must not raise an alarm.
    /// Without our own sink there is no adapter in the rack, so there is
    /// nothing a plug could be seated against.
    /// The failure the panel could not previously see: OUTPUT went on saying
    /// "Muh Chickin Waffles" while every sample went to EasyEffects instead.
    /// `own_output_is_looping` answers "no" here — correctly, it is not a
    /// loop — which is exactly why this needs asking separately.
    /// Unplug the headphones the rack says it drives and the sound goes to
    /// whatever PipeWire falls back to. The name in OUTPUT is then a claim
    /// about a device that is not in the machine — the same false assertion
    /// this whole change exists to stop, so it has to raise the alarm rather
    /// than read as the ordinary end of something.
    /// or not yet started. Nothing to be wrong about yet.
    /// Following the system default is not a claim about any device, so there
    /// is nothing to be wrong about.
    /// `want` is a description and sinks are compared by index; the panel
    /// stores descriptions and PipeWire routes by name. Matching one against
    /// the other is this codebase's favourite mistake, so a sink whose *name*
    /// happens to equal the wanted *description* must not satisfy the test.
    /// Both subjects are read from one graph, and they are independent: the
    /// aux plug can hold while the output does not, which is precisely the
    /// state that had the rack processing audio nobody could hear.
    /// A plug that has come out on its own gets pushed back in, once, without
    /// bothering anybody about it.
    /// Something that moves it straight back is contesting it, and the second
    /// exchange is where the rack concedes. Three, four and five must not
    /// produce further attempts: that is the stuttering war.
    /// The case that made this necessary. EasyEffects is holding the stream,
    /// the rack has conceded — and then EasyEffects quits. Its sink goes with
    /// it, WirePlumber tips the orphan onto the system default, and the stream
    /// is now sitting somewhere nobody is defending. A rack that stayed
    /// conceded would leave it there forever.
    /// Our own output stream must never be reported adrift, whatever the index
    /// says. A sink-input index is the server's and can be handed to somebody
    /// else once our stream ends; if that somebody were us, "recovering" it
    /// would move the rack's output onto the rack's own input — the feedback
    /// loop the rest of this file works hardest to prevent, caused by the guard
    /// meant to keep the path honest.
    /// The point of #10: an application that rebuilt its playback stream is the
    /// same plug at a new index, and gets picked up without anybody being asked.
    /// Chrome does this every few minutes; a track change is enough for some
    /// players. Watching only the number loses the plug the first time it
    /// happens, and the bay then describes a path belonging to a stream that no
    /// longer exists.
    /// Adoption must not reach past the operator's choice. A different
    /// application playing is not the plug coming back.
    /// The two memories must never be crossed. A drifting output asking for the
    /// aux stream to be moved — or the reverse — would be a guard doing the
    /// wrong thing entirely while every isolated test still passed.
    /// A failed read is not an observation. Feeding it through as one would
    /// hand back an attempt that was deliberately spent, and the rack would
    /// re-arm against a grabber it had already conceded to — a `pactl` that
    /// fails every few minutes turning into a move every other second.
    /// Plugging in something else is a new question. Without this, choosing
    /// Spotify after conceding Chrome to a grabber would report the new plug
    /// contested having never once tried to seat it.
    /// A stream that ended asks to be dropped *by name*, so the caller can
    /// check it is still the plug in question.
    ///
    /// Judging a tick means reading the index, asking the server two questions
    /// about it, and only then deciding — with no lock held across any of it.
    /// The operator can plug something new in during that window; clearing
    /// blind would throw their plug away, and the guard would then watch
    /// nothing at all while the bay went on describing a signal path. Silent,
    /// permanent, and exactly the failure this whole mechanism exists to stop.
    /// Choosing a different output device is a new question, exactly as
    /// choosing a different stream is. Without this the new device inherits a
    /// concession made about the old one and is reported lost without a single
    /// attempt — the same defect as for the aux plug, on the other subject.
    /// Two sinks can share a description — a pair of identical interfaces, or
    /// two HDMI outputs both called "Digital Output". Identity is the index, so
    /// a stream that lands on the *other* one of a same-named pair is a new
    /// mishap and is owed an attempt. Keying on the string would silently
    /// forfeit exactly the recovery this mechanism exists for.
    /// A grabber slower than the guard's own clock must not get a fresh attempt
    /// every time it pauses for breath.
    ///
    /// The rule is "one exchange", not "one exchange per second". Seeing the
    /// plug seated once is not the same as it holding: something that takes the
    /// stream every few ticks would otherwise be traded with forever, quietly
    /// enough to be mistaken for a glitch. The sequence below is the one the
    /// unbroken-`Adrift` test cannot reach.
    /// But a plug that genuinely settles gets its attempt back, or a stream
    /// grabbed once at lunchtime would be un-defended for the rest of the day.
    /// A fixed threshold cannot end the fight, only move it: against a grabber
    /// slower than the threshold it trades one move per period *forever*. So
    /// the wait doubles per exchange with the same sink. A one-off mishap still
    /// recovers immediately; something that keeps taking the same stream is
    /// given up on at a rate the operator hears receding rather than as a
    /// permanent tic.
    /// Ending the stream clears the slate: the next plug starts fresh, rather
    /// than inheriting an attempt spent on whatever happened last time.
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
