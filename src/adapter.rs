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

/// Send this process's own audio to `sink`.
///
/// Moving our own sink-input is how the rack holds an output independent of
/// the system default, and it takes effect immediately — no stream rebuild,
/// no restart. It is the same `move-sink-input` used to plug a stream into
/// a stream in, pointed the other way.
pub fn route_own_output(sink: &str) -> Result<()> {
    let json = pactl(&["-f", "json", "list", "sink-inputs"])?;
    let v: serde_json::Value = serde_json::from_str(&json)?;
    let Some(items) = v.as_array() else { bail!("no streams") };

    let mine = items
        .iter()
        .find(|s| s.get("properties").is_some_and(is_ours))
        .and_then(|s| s.get("index"))
        .and_then(|i| i.as_u64());

    let Some(index) = mine else { bail!("our own output stream is not on the graph yet") };
    pactl(&["move-sink-input", &index.to_string(), sink])?;
    Ok(())
}

/// The sink the desktop currently sends everything to, by internal name.
pub fn default_sink() -> Option<String> {
    let out = Command::new("pactl").args(["get-default-sink"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Somewhere safe for the rack's own output to land.
///
/// In order: the remembered device, the system default, then anything at all —
/// and never our own input sink at any of those steps. `preferred` is a
/// description, because that is what the picker shows and what the memory
/// stores; the name is what PipeWire wants.
pub fn safe_output(preferred: Option<&str>) -> Option<Sink> {
    // `sinks()` already refuses to list our own, so anything it returns is
    // safe by construction. The default has to be checked by hand.
    let all = sinks();
    if let Some(want) = preferred
        && let Some(s) = all.iter().find(|s| s.description == want)
    {
        return Some(s.clone());
    }
    if let Some(d) = default_sink()
        && let Some(s) = all.iter().find(|s| s.name == d)
    {
        return Some(s.clone());
    }
    all.into_iter().next()
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
    let get = |k: &str| props.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let identity = format!("{} {}", get("node.name"), get("application.name"));
    if identity.contains(env!("CARGO_PKG_NAME")) {
        return true;
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    !exe.is_empty() && identity.contains(&exe)
}

/// Whether something we routed is still where we put it.
///
/// `move-sink-input` is a request, not a setting. It has no memory: the server
/// honours it once, and any policy agent on the desktop may move the stream
/// straight back, on its own schedule, for its own reasons. EasyEffects with
/// "process all output streams" on does exactly that, about once a second, and
/// the request is undone within ~300 ms of being granted.
///
/// So a plug is not a fact until it has been looked at again. This is the
/// looking.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Link {
    /// Nothing has been routed, so there is nothing to hold.
    #[default]
    Idle,
    /// On the sink we asked for.
    Seated,
    /// Somewhere else, named by where it is sitting.
    ///
    /// Stated as a *location*, not an accusation, because most of the time
    /// nothing did this on purpose: when a virtual sink disappears — its owner
    /// quit — WirePlumber drops the orphaned streams onto the system default,
    /// and the device they land on is a bystander. Saying a pair of headphones
    /// "took" a stream is the panel inventing an actor.
    Adrift(String),
    /// Somewhere else *again*, after the rack pushed the plug back in.
    ///
    /// This is the one that has earned a name. Something is actively holding
    /// the stream — it moved it back within the second — and no number of
    /// further attempts will win, because both processes have equal
    /// privileges. The rack stops trying and says who.
    Contested(String),
    /// What we routed is no longer on the graph — the stream ended, or the
    /// device went away. Ordinary, and not a fault.
    Gone,
}

impl Link {
    /// Whether the signal is not going where the panel says it is. `Gone` is
    /// not: a stream that ended is the normal end of listening to something.
    pub fn astray(&self) -> bool {
        matches!(self, Link::Adrift(_) | Link::Contested(_))
    }
}

/// Push the plug back in: one `move-sink-input`, same as the original.
pub fn reseat(index: u32) -> Result<()> {
    pactl(&["move-sink-input", &index.to_string(), SINK])?;
    Ok(())
}

/// One attempt, and then the truth.
///
/// A plug that has come out is worth pushing back in exactly once. If it holds,
/// nothing was fighting for it and the operator never needs to know. If it does
/// not, something is, and pushing a third and fourth time is a war between two
/// processes with equal privileges — which neither wins, and which the operator
/// hears as stuttering.
///
/// The attempt is remembered *against the sink it was found on*, which is what
/// makes recovery work. A grabber holding the stream keeps returning it to the
/// same place, so the attempt is spent and stays spent. When that grabber quits,
/// the stream falls somewhere new — a different sink, so a fresh attempt is
/// owed, and the rack takes it back without being asked.
#[derive(Debug, Default)]
pub struct Reseat {
    spent_on: Option<String>,
}

impl Reseat {
    /// What to report, and whether to push the plug in before reporting it.
    pub fn judge(&mut self, seen: Link) -> (Link, bool) {
        match seen {
            Link::Adrift(where_it_is) => {
                if self.spent_on.as_deref() == Some(where_it_is.as_str()) {
                    // Tried that already, and here it is again.
                    (Link::Contested(where_it_is), false)
                } else {
                    self.spent_on = Some(where_it_is.clone());
                    (Link::Adrift(where_it_is), true)
                }
            }
            // Back where it belongs, so the next mishap is a fresh one.
            other => {
                self.spent_on = None;
                (other, false)
            }
        }
    }
}

/// Both routing questions, answered together.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Routing {
    /// The stream plugged into the aux input.
    pub aux: Link,
    /// The rack's own output, against the device the panel claims it drives.
    pub output: Link,
}

/// Ask both questions from one pair of reads.
///
/// They are asked on the same clock and each needs the same two lists, so
/// asking them separately would spawn four `pactl` processes a second to
/// describe one graph. `plugged` is a sink-input index; `want` is an output
/// *description*, because that is what the picker shows and what the 12-volt
/// memory stores — see [`safe_output`].
pub fn routing(plugged: Option<u32>, want: Option<&str>) -> Routing {
    let Ok(ij) = pactl(&["-f", "json", "list", "sink-inputs"]) else { return Routing::default() };
    let Ok(sj) = pactl(&["-f", "json", "list", "sinks"]) else { return Routing::default() };
    let (Ok(iv), Ok(sv)) = (
        serde_json::from_str::<serde_json::Value>(&ij),
        serde_json::from_str::<serde_json::Value>(&sj),
    ) else {
        return Routing::default();
    };
    let (Some(inputs), Some(sinks)) = (iv.as_array(), sv.as_array()) else {
        return Routing::default();
    };
    decide(inputs, sinks, plugged, want)
}

/// The decision, separated from the fetching so it can be tested against a
/// graph that is actually broken rather than only against a healthy desktop.
fn decide(
    inputs: &[serde_json::Value],
    sinks: &[serde_json::Value],
    plugged: Option<u32>,
    want: Option<&str>,
) -> Routing {
    let index_of = |s: &serde_json::Value| s.get("index").and_then(|i| i.as_u64());
    let describe = |idx: u64| -> String {
        sinks
            .iter()
            .find(|s| index_of(s) == Some(idx))
            .and_then(|s| {
                s.get("description")
                    .and_then(|d| d.as_str())
                    .or_else(|| s.get("name").and_then(|n| n.as_str()))
            })
            .unwrap_or("another output")
            .to_string()
    };
    // Where a given sink-input actually sits, if it is still on the graph.
    let sink_of = |pred: &dyn Fn(&serde_json::Value) -> bool| -> Option<u64> {
        inputs.iter().find(|i| pred(i)).and_then(|i| i.get("sink")).and_then(|s| s.as_u64())
    };
    let named = |name: &str| -> Option<u64> {
        sinks
            .iter()
            .find(|s| s.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(index_of)
    };

    // The plugged stream, against our own sink.
    let aux = match (plugged, named(SINK)) {
        (Some(idx), Some(ours)) => {
            match sink_of(&|i| index_of(i) == Some(idx as u64)) {
                None => Link::Gone,
                Some(on) if on == ours => Link::Seated,
                Some(on) => Link::Adrift(describe(on)),
            }
        }
        // No sink of ours means the adapter is not in, so nothing is plugged.
        _ => Link::Idle,
    };

    // Our own output, against the device the panel claims. `want` is matched on
    // description and the answer is compared by sink index — matching the
    // description against a *name* would be the same class of mistake this
    // whole check exists to catch.
    let output = match want {
        None => Link::Idle,
        Some(desc) => {
            let target = sinks
                .iter()
                .find(|s| s.get("description").and_then(|d| d.as_str()) == Some(desc))
                .and_then(index_of);
            match (target, sink_of(&|i| i.get("properties").is_some_and(is_ours))) {
                // The device the memory names is not on the system any more.
                (None, _) => Link::Gone,
                (Some(_), None) => Link::Gone,
                (Some(t), Some(on)) if on == t => Link::Seated,
                (Some(_), Some(on)) => Link::Adrift(describe(on)),
            }
        }
    };

    Routing { aux, output }
}

/// Is the rack's own output sitting on the rack's own input?
///
/// The desktop can put it there at any moment — "move all applications to
/// ten-qd" in the system settings does exactly that, and it is a reasonable
/// thing to click. Nothing about our own configuration prevents it, so the
/// only honest answer is to keep checking.
pub fn own_output_is_looping() -> bool {
    let Ok(json) = pactl(&["-f", "json", "list", "sink-inputs"]) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else { return false };
    let Some(items) = v.as_array() else { return false };

    // The sink index our own sink carries, if it is loaded at all.
    let Ok(sj) = pactl(&["-f", "json", "list", "sinks"]) else { return false };
    let Ok(sv) = serde_json::from_str::<serde_json::Value>(&sj) else { return false };
    let ours = sv.as_array().and_then(|a| {
        a.iter()
            .find(|s| s.get("name").and_then(|n| n.as_str()) == Some(SINK))
            .and_then(|s| s.get("index"))
            .and_then(|i| i.as_u64())
    });
    let Some(ours) = ours else { return false };

    items.iter().any(|s| {
        s.get("properties").is_some_and(is_ours)
            && s.get("sink").and_then(|x| x.as_u64()) == Some(ours)
    })
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
    /// The rack's own input must never be offered as its output. That is the
    /// one choice in the picker that makes a feedback loop, and the guard is
    /// what keeps the watchdog's fallback safe by construction.
    #[test]
    fn the_output_picker_never_offers_our_own_input() {
        assert!(
            !sinks().iter().any(|s| s.name == SINK),
            "our own sink was offered as somewhere to send sound"
        );
    }

    /// Whatever it settles on, it is never the loop.
    /// The graph as it actually stood on the machine where this was found:
    /// EasyEffects holding every stream, including ours, while the panel
    /// claimed the Bluetooth headphones. Indices are the real ones.
    fn stolen() -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let sinks = vec![
            serde_json::json!({
                "index": 47, "name": "easyeffects_sink", "description": "EasyEffects Sink"
            }),
            serde_json::json!({
                "index": 87446,
                "name": "bluez_output.68_F2_1F_05_94_2F.1",
                "description": "Muh Chickin Waffles"
            }),
            serde_json::json!({ "index": 87671, "name": SINK, "description": DESCRIPTION }),
        ];
        let inputs = vec![
            serde_json::json!({
                "index": 88131, "sink": 47,
                "properties": { "node.name": "Google Chrome", "application.name": "Google Chrome" }
            }),
            serde_json::json!({
                "index": 87605, "sink": 47,
                "properties": {
                    "node.name": "alsa_playback.ten-qd",
                    "application.name": "PipeWire ALSA [ten-qd]"
                }
            }),
        ];
        (inputs, sinks)
    }

    /// The same graph with both streams where they were asked to be.
    fn healthy() -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let (mut inputs, sinks) = stolen();
        inputs[0]["sink"] = serde_json::json!(87671); // Chrome, on the aux sink
        inputs[1]["sink"] = serde_json::json!(87446); // us, on the headphones
        (inputs, sinks)
    }

    #[test]
    fn a_plug_that_held_reads_seated() {
        let (i, s) = healthy();
        assert_eq!(decide(&i, &s, Some(88131), None).aux, Link::Seated);
    }

    /// The whole point. `move-sink-input` was granted and then undone, and the
    /// panel has to be able to say so — and say by whom, because "it did not
    /// work" sends you hunting through your own gain structure.
    #[test]
    fn a_plug_that_was_taken_back_names_who_took_it() {
        let (i, s) = stolen();
        assert_eq!(
            decide(&i, &s, Some(88131), None).aux,
            Link::Adrift("EasyEffects Sink".into())
        );
    }

    /// A stream that ended is not a fault, and must not light a warning: that
    /// is what stopping the music looks like from here.
    #[test]
    fn a_stream_that_ended_is_gone_rather_than_pulled() {
        let (i, s) = healthy();
        let r = decide(&i, &s, Some(9999), None);
        assert_eq!(r.aux, Link::Gone);
        assert!(!r.aux.astray(), "a finished stream must not raise an alarm");
    }

    #[test]
    fn nothing_plugged_in_is_idle() {
        let (i, s) = healthy();
        assert_eq!(decide(&i, &s, None, None).aux, Link::Idle);
    }

    /// Without our own sink there is no adapter in the rack, so there is
    /// nothing a plug could be seated against.
    #[test]
    fn no_aux_sink_means_nothing_can_be_plugged() {
        let (i, mut s) = healthy();
        s.retain(|x| x["name"] != SINK);
        assert_eq!(decide(&i, &s, Some(88131), None).aux, Link::Idle);
    }

    #[test]
    fn an_output_the_rack_really_drives_reads_seated() {
        let (i, s) = healthy();
        assert_eq!(decide(&i, &s, None, Some("Muh Chickin Waffles")).output, Link::Seated);
    }

    /// The failure the panel could not previously see: OUTPUT went on saying
    /// "Muh Chickin Waffles" while every sample went to EasyEffects instead.
    /// `own_output_is_looping` answers "no" here — correctly, it is not a
    /// loop — which is exactly why this needs asking separately.
    #[test]
    fn an_output_that_was_re_homed_says_where_it_went() {
        let (i, s) = stolen();
        assert_eq!(
            decide(&i, &s, None, Some("Muh Chickin Waffles")).output,
            Link::Adrift("EasyEffects Sink".into())
        );
    }

    #[test]
    fn an_output_device_that_went_away_is_gone() {
        let (i, mut s) = healthy();
        s.retain(|x| x["description"] != "Muh Chickin Waffles");
        assert_eq!(decide(&i, &s, None, Some("Muh Chickin Waffles")).output, Link::Gone);
    }

    /// Following the system default is not a claim about any device, so there
    /// is nothing to be wrong about.
    #[test]
    fn following_the_default_makes_no_claim() {
        let (i, s) = stolen();
        assert_eq!(decide(&i, &s, None, None).output, Link::Idle);
    }

    /// `want` is a description and sinks are compared by index; the panel
    /// stores descriptions and PipeWire routes by name. Matching one against
    /// the other is this codebase's favourite mistake, so a sink whose *name*
    /// happens to equal the wanted *description* must not satisfy the test.
    #[test]
    fn a_name_that_looks_like_the_wanted_description_does_not_count() {
        let (i, mut s) = healthy();
        s.push(serde_json::json!({
            "index": 5150, "name": "Muh Chickin Waffles", "description": "something else"
        }));
        // We are on 87446, whose *description* is the one asked for. Still
        // seated — the impostor's matching name must not steal the comparison.
        assert_eq!(decide(&i, &s, None, Some("Muh Chickin Waffles")).output, Link::Seated);
    }

    /// Both subjects are read from one graph, and they are independent: the
    /// aux plug can hold while the output does not, which is precisely the
    /// state that had the rack processing audio nobody could hear.
    #[test]
    fn the_two_questions_are_answered_separately() {
        let (mut i, s) = stolen();
        i[0]["sink"] = serde_json::json!(87671); // Chrome plugged in fine
        let r = decide(&i, &s, Some(88131), Some("Muh Chickin Waffles"));
        assert_eq!(r.aux, Link::Seated);
        assert!(r.output.astray(), "the output is not where the panel says");
    }

    /// A plug that has come out on its own gets pushed back in, once, without
    /// bothering anybody about it.
    #[test]
    fn a_plug_that_came_out_is_pushed_back_in() {
        let mut r = Reseat::default();
        let (said, push) = r.judge(Link::Adrift("Muh Chickin Waffles".into()));
        assert!(push, "a first drift is worth one attempt");
        assert_eq!(said, Link::Adrift("Muh Chickin Waffles".into()));
        // And it held.
        assert_eq!(r.judge(Link::Seated), (Link::Seated, false));
    }

    /// Something that moves it straight back is contesting it, and the second
    /// exchange is where the rack concedes. Three, four and five must not
    /// produce further attempts: that is the stuttering war.
    #[test]
    fn a_stream_something_else_is_holding_is_conceded_after_one_exchange() {
        let mut r = Reseat::default();
        let ee = Link::Adrift("EasyEffects Sink".into());
        assert!(r.judge(ee.clone()).1, "the first attempt is always owed");
        for round in 0..4 {
            let (said, push) = r.judge(ee.clone());
            assert!(!push, "round {round} tried again — that is the war");
            assert_eq!(said, Link::Contested("EasyEffects Sink".into()));
        }
    }

    /// The case that made this necessary. EasyEffects is holding the stream,
    /// the rack has conceded — and then EasyEffects quits. Its sink goes with
    /// it, WirePlumber tips the orphan onto the system default, and the stream
    /// is now sitting somewhere nobody is defending. A rack that stayed
    /// conceded would leave it there forever.
    #[test]
    fn conceding_to_one_grabber_does_not_forfeit_the_next_recovery() {
        let mut r = Reseat::default();
        r.judge(Link::Adrift("EasyEffects Sink".into()));
        assert_eq!(
            r.judge(Link::Adrift("EasyEffects Sink".into())).0,
            Link::Contested("EasyEffects Sink".into()),
            "conceded, as it should be"
        );
        // EasyEffects quits; the stream falls to the default output.
        let (said, push) = r.judge(Link::Adrift("Muh Chickin Waffles".into()));
        assert!(push, "a different sink is a new mishap and is owed an attempt");
        assert_eq!(said, Link::Adrift("Muh Chickin Waffles".into()));
        assert_eq!(r.judge(Link::Seated), (Link::Seated, false), "and it came home");
    }

    /// A device the operator is not fighting over must never be blamed. The
    /// headphones a stream landed on did not take it — the sink that vanished
    /// did — and the panel says so only when it has evidence.
    #[test]
    fn a_bystander_sink_is_never_accused() {
        let mut r = Reseat::default();
        let (said, _) = r.judge(Link::Adrift("Muh Chickin Waffles".into()));
        assert!(
            !matches!(said, Link::Contested(_)),
            "a first landing is a location, not a culprit"
        );
    }

    /// Ending the stream clears the slate: the next plug starts fresh, rather
    /// than inheriting an attempt spent on whatever happened last time.
    #[test]
    fn a_stream_that_ends_spends_no_attempt() {
        let mut r = Reseat::default();
        r.judge(Link::Adrift("EasyEffects Sink".into()));
        r.judge(Link::Gone);
        assert!(
            r.judge(Link::Adrift("EasyEffects Sink".into())).1,
            "a fresh plug is owed its own attempt"
        );
    }

    #[test]
    fn a_safe_output_is_never_our_own_input() {
        for want in [None, Some(DESCRIPTION), Some("no such device")] {
            if let Some(s) = safe_output(want) {
                assert_ne!(s.name, SINK, "asked for {want:?} and got the loop");
            }
        }
    }

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
