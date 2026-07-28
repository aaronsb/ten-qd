//! Whether the signal actually went down the wire.
//!
//! The parent module is about what the wire *is* — a sink, the streams that can
//! be plugged into it, the lifecycle of the thing appearing in everyone's audio
//! settings. This is about the other half of the question, and it turned out to
//! be a different subject with a different shape: `move-sink-input` is a request
//! with no memory, so every routing this program asks for is a claim that stops
//! being true the moment some other program decides otherwise.
//!
//! So nothing here trusts an instruction to have taken. [`Link`] is the second
//! look, [`Reseat`] is what to do about a plug that has come out, and [`Guard`]
//! is the two of them wired together — returning intents rather than performing
//! moves, because which decision drives which move is the part that was hardest
//! to get right and is worth being able to read in a test.

use anyhow::{bail, Result};

use std::process::Command;

use super::{app_of, is_ours, pactl, sinks, Plug, Sink, QUIESCED, SINK};

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
/// A sink something ended up on: what the graph calls it, and what to call it
/// on the panel.
///
/// Both, because they answer different questions. The **index** is identity —
/// it is what the state machine remembers and compares, because it is what
/// PipeWire actually distinguishes. The **description** is only ever displayed.
/// Two identical USB interfaces, or a pair of HDMI outputs both called "Digital
/// Output", share a description while being entirely different places for audio
/// to go; keying on the string would have a grabber's sink and the sink a
/// stream later landed on compare equal, and the recovery this whole mechanism
/// exists for would silently never happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Where {
    pub sink: u64,
    pub desc: String,
}

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
    Adrift(Where),
    /// Somewhere else *again*, after the rack pushed the plug back in.
    ///
    /// This is the one that has earned a name. Something is actively holding
    /// the stream — it moved it back within the second — and no number of
    /// further attempts will win, because both processes have equal
    /// privileges. The rack stops trying and says who.
    Contested(Where),
    /// The device the panel names is not on the system at all.
    ///
    /// Only ever the output's answer. A stream ending is ordinary; a *device*
    /// the panel is still naming having left the machine is not, and it is the
    /// same false claim in a different costume — unplug the headphones the rack
    /// says it drives and the name would otherwise sit there in white while
    /// every sample goes to whatever PipeWire fell back to.
    Absent,
    /// What we routed is no longer on the graph — the stream ended. Ordinary,
    /// and not a fault: it is what stopping the music looks like from here.
    Gone,
    /// Not examined. The guard spent this tick on something more urgent.
    ///
    /// Distinct from `Idle`, which is a *finding* — looked, nothing plugged in,
    /// no claim to check. This is the absence of a finding, and the panel has to
    /// render the difference: a reading nobody took must not be drawn as a
    /// reading that came back clean, or the rack looks healthy precisely when it
    /// has stopped checking itself.
    Unknown,
}

impl Link {
    /// Whether the signal is not going where the panel says it is. `Gone` is
    /// not: a stream that ended is the normal end of listening to something.
    pub fn astray(&self) -> bool {
        matches!(self, Link::Adrift(_) | Link::Contested(_) | Link::Absent)
    }

    /// Where it went, for the panel to print. `None` when there is nowhere to
    /// name — nothing is wrong, or the place itself is what has gone missing.
    pub fn desc(&self) -> Option<&str> {
        match self {
            Link::Adrift(w) | Link::Contested(w) => Some(&w.desc),
            _ => None,
        }
    }
}

/// Push the plug back in: one `move-sink-input`, same as the original.
///
/// Refuses once the adapter is on its way out. The guard thread is detached and
/// outlives everything, so "we are past the point of moving streams around" has
/// to be a fact it can read rather than a promise about ordering.
pub fn reseat(index: u32) -> Result<()> {
    if QUIESCED.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(());
    }
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
    /// The sink an attempt is currently spent on, by index — see [`Where`] for
    /// why not by name. `None` means one is owed.
    spent_on: Option<u64>,
    /// The sink being fought over, and how many attempts have been spent on it.
    ///
    /// Separate from `spent_on`, and it has to be: the count survives the plug
    /// settling, which is the whole point. Losing it there would reset the
    /// patience every time the grabber paused, and the wait would never grow.
    fought: Option<(u64, u32)>,
    /// Consecutive sightings of the plug sitting where it belongs.
    held: u32,
}

/// How long the plug has to stay put before the attempt spent on it is
/// returned — thirty sightings, so about thirty seconds at the guard's clock.
///
/// Seeing it seated *once* is not the same as it holding. A grabber slower than
/// the guard's own second — one that acts on a device change, or every few
/// seconds — would otherwise be handed a fresh attempt every time it paused for
/// breath, and the two processes would trade moves for as long as the program
/// ran: exactly the war the single attempt exists to avoid, only quiet enough
/// to be mistaken for a glitch.
const SETTLED: u32 = 30;

/// How many times the threshold doubles before it stops growing — 30, 60, 120,
/// 240 sightings, so a fourth exchange with the same grabber buys about four
/// minutes of quiet and an eighth is conceded in all but name.
const PATIENCE: u32 = 3;

impl Reseat {
    /// How long the plug must hold before a fresh attempt is owed on this sink.
    ///
    /// It grows, because a fixed threshold cannot remove the fight — only move
    /// it. Against a grabber slower than the threshold, a fixed value trades one
    /// move per period *forever*; the exchanges are bounded and self-announcing,
    /// but they never stop. Doubling per exchange makes the rate decay instead:
    /// a one-off mishap is still recovered immediately, and something that keeps
    /// taking the same stream is given up on at a rate the operator can hear
    /// receding rather than as a permanent tic.
    fn threshold(&self) -> u32 {
        let rounds = self.fought.map_or(1, |(_, n)| n.max(1));
        SETTLED << (rounds - 1).min(PATIENCE)
    }

    /// What to report, and whether to push the plug in before reporting it.
    pub fn judge(&mut self, seen: Link) -> (Link, bool) {
        match seen {
            Link::Adrift(where_it_is) => {
                self.held = 0;
                if self.spent_on == Some(where_it_is.sink) {
                    // Tried that already, and here it is again.
                    return (Link::Contested(where_it_is), false);
                }
                // A fresh attempt, and the count of how many this sink has now
                // cost — which is what makes the next wait longer than this one.
                self.fought = match self.fought {
                    Some((sink, n)) if sink == where_it_is.sink => {
                        Some((sink, n.saturating_add(1)))
                    }
                    _ => Some((where_it_is.sink, 1)),
                };
                self.spent_on = Some(where_it_is.sink);
                (Link::Adrift(where_it_is), true)
            }
            Link::Seated => {
                self.held = self.held.saturating_add(1);
                if self.held >= self.threshold() {
                    self.spent_on = None;
                }
                (Link::Seated, false)
            }
            // Nothing plugged, or the stream ended. Whatever comes next is a
            // different question and is owed its own attempt — and a different
            // fight, so the patience built up over the last one goes too.
            other => {
                self.spent_on = None;
                self.fought = None;
                self.held = 0;
                (other, false)
            }
        }
    }
}

/// What one look at the graph found: the two readings, and whether the plug
/// has moved to a stream the guard was not yet watching.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Verdict {
    pub routing: Routing,
    /// The sink-input the plugged application lives on now, when that is not
    /// the one being watched.
    pub successor: Option<u32>,
}

/// Both routing questions, answered together.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Routing {
    /// The stream plugged into the aux input.
    pub aux: Link,
    /// The rack's own output, against the device the panel claims it drives.
    pub output: Link,
    /// The rack's output was found sitting on the rack's own input, and moved
    /// off it. The one condition the guard fights rather than reports — and,
    /// until now, the one it fought entirely in silence.
    pub howling: bool,
}

/// Something the guard has decided to do about the graph.
///
/// Returned rather than performed, so that *which* decision drives *which*
/// move is a thing a test can read. The three ways this wiring can be wrong —
/// never pushing at all, pushing on every drift, and crossing the two memories
/// over — are all invisible to a test that can only see the two halves apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Push this sink-input back onto our sink.
    Reseat(u32),
    /// Put our own output back on the device the panel names.
    Reroute,
    /// Stop watching this sink-input: the stream it named has ended.
    ///
    /// Carries the index it is about, and the caller must check that this is
    /// still the index being watched before acting. Judging a tick takes two
    /// `pactl` round-trips with no lock held, and the operator can plug
    /// something new in during that window — a blind clear would then throw
    /// away the plug they just made, leaving the guard watching nothing while
    /// the bay went on describing a signal path.
    Unwatch(u32),
    /// Watch this sink-input instead: the application moved to a new stream.
    Adopt(u32),
}

/// The routing guard's whole decision, per tick.
///
/// Two [`Reseat`] memories that must not be confused with one another, plus the
/// index they are about — because a different stream is a different plug, owed
/// its own attempt rather than inheriting one spent on whatever came before.
#[derive(Debug, Default)]
pub struct Guard {
    aux: Reseat,
    out: Reseat,
    /// The plug each memory is about. An attempt is spent on *a particular
    /// thing being in a particular place*, so when either subject changes the
    /// memory of it has to go — otherwise the new one inherits a concession
    /// made about the old and is reported lost without one attempt being made.
    watching: Option<u32>,
    driving: Option<String>,
}

impl Guard {
    /// One tick: what to report, and what to do about it.
    ///
    /// `raw` is `None` when the graph could not be read. Nothing is reported
    /// and nothing is done — a failed read is not an observation, and feeding
    /// it to [`Reseat`] as though it were would hand back an attempt that was
    /// deliberately spent.
    pub fn tick(
        &mut self,
        raw: Option<Verdict>,
        plugged: Option<&Plug>,
        driving: Option<&str>,
    ) -> (Option<Routing>, Vec<Act>) {
        let Some(raw) = raw else { return (None, Vec::new()) };
        let mut acts = Vec::new();

        // The stream this tick is actually about — the one being watched, or
        // the one that replaced it. Everything below is about *that* index, so
        // that adopting a successor and seating it can happen in one tick
        // rather than leaving the bay wrong for a second in between.
        let about = raw.successor.or(plugged.map(|p| p.index));
        if let Some(i) = raw.successor {
            acts.push(Act::Adopt(i));
        }

        if about != self.watching {
            self.aux = Reseat::default();
            self.watching = about;
        }
        if driving != self.driving.as_deref() {
            self.out = Reseat::default();
            self.driving = driving.map(str::to_string);
        }

        let (aux, push) = self.aux.judge(raw.routing.aux);
        if push && let Some(i) = about {
            acts.push(Act::Reseat(i));
        }
        if aux == Link::Gone && let Some(p) = plugged {
            acts.push(Act::Unwatch(p.index));
        }
        let (output, reroute) = self.out.judge(raw.routing.output);
        if reroute {
            acts.push(Act::Reroute);
        }
        (Some(Routing { aux, output, ..Default::default() }), acts)
    }
}

/// Ask both questions from one pair of reads. `None` if the graph could not be
/// read at all.
///
/// They are asked on the same clock and each needs the same two lists, so
/// asking them separately would spawn four `pactl` processes a second to
/// describe one graph. `plugged` is a sink-input index; `want` is an output
/// *description*, because that is what the picker shows and what the 12-volt
/// memory stores — see [`safe_output`].
///
/// **A failed read is not an observation.** This returned `Routing::default()`
/// on any error, which is `Idle` on both counts — indistinguishable from a
/// healthy "nothing plugged in, no claim made". [`Reseat`] reads `Idle` as
/// "back where it belongs" and returns the attempt it had spent, so a single
/// failed `pactl` — a pipewire-pulse restart, a `fork` refused under load, and
/// this forks four processes a second — would re-arm the rack against a grabber
/// it had already conceded to, and the two would trade a move every other
/// second for the rest of the run. The caller has to be able to tell "I looked
/// and nothing is wrong" from "I could not look".
pub fn routing(plugged: Option<&Plug>, want: Option<&str>) -> Option<Verdict> {
    let (Ok(ij), Ok(sj)) = (
        pactl(&["-f", "json", "list", "sink-inputs"]),
        pactl(&["-f", "json", "list", "sinks"]),
    ) else {
        return None;
    };
    let (Ok(iv), Ok(sv)) = (
        serde_json::from_str::<serde_json::Value>(&ij),
        serde_json::from_str::<serde_json::Value>(&sj),
    ) else {
        return None;
    };
    let (Some(inputs), Some(sinks)) = (iv.as_array(), sv.as_array()) else {
        return None;
    };
    Some(decide(inputs, sinks, plugged, want))
}

/// The decision, separated from the fetching so it can be tested against a
/// graph that is actually broken rather than only against a healthy desktop.
fn decide(
    inputs: &[serde_json::Value],
    sinks: &[serde_json::Value],
    plugged: Option<&Plug>,
    want: Option<&str>,
) -> Verdict {
    let index_of = |s: &serde_json::Value| s.get("index").and_then(|i| i.as_u64());
    // Identity is the index; the description is only what to print. A sink
    // destroyed between the two reads above is in the inputs list and not the
    // sinks list, so the name can genuinely be unknown — but the index still
    // distinguishes it, which is why `Where` carries both.
    let put = |idx: u64| -> Where {
        let desc = sinks
            .iter()
            .find(|s| index_of(s) == Some(idx))
            .and_then(|s| {
                s.get("description")
                    .and_then(|d| d.as_str())
                    .or_else(|| s.get("name").and_then(|n| n.as_str()))
            })
            .unwrap_or("another output")
            .to_string();
        Where { sink: idx, desc }
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

    // Anything that is not one of ours. A sink-input index is the server's, not
    // ours: the stream we plugged can end at any moment and the number be handed
    // to somebody else — and if that somebody is our own output, reporting it
    // adrift would have the guard "recover" it onto our own input, which is the
    // feedback loop every other guard in this file exists to prevent. The index
    // is a weak identity, so it does not get to name us.
    let theirs = |i: &serde_json::Value| !i.get("properties").is_some_and(is_ours);

    // The plugged stream, against our own sink.
    //
    // Resolved by index first and by *application* second. An application tears
    // down and rebuilds its playback stream constantly — a track change is
    // enough for some — and each rebuild is a new index. What the operator
    // chose was the application; the index is only where it lived at the time.
    // So when the tracked index is gone, the stream that replaced it is the
    // same plug and gets picked up without anybody being asked.
    let mut successor = None;
    let aux = match (plugged, named(SINK)) {
        (Some(plug), Some(ours)) => {
            let live = inputs
                .iter()
                .find(|i| index_of(i) == Some(plug.index as u64) && theirs(i))
                .or_else(|| {
                    inputs.iter().find(|i| {
                        theirs(i)
                            && i.get("properties").map(app_of).as_deref() == Some(&plug.app)
                    })
                });
            match live {
                None => Link::Gone,
                Some(i) => {
                    if index_of(i) != Some(plug.index as u64) {
                        successor = index_of(i).map(|x| x as u32);
                    }
                    match i.get("sink").and_then(|s| s.as_u64()) {
                        Some(on) if on == ours => Link::Seated,
                        Some(on) => Link::Adrift(put(on)),
                        None => Link::Gone,
                    }
                }
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
            // Resolved over the same universe of sinks `safe_output` will pick
            // from — which excludes our own. Otherwise a remembered description
            // that happened to name our input would resolve to a target
            // `safe_output` can never return, and the output would latch
            // Contested for the rest of the run with nothing able to clear it.
            let target = sinks
                .iter()
                .filter(|s| s.get("name").and_then(|n| n.as_str()) != Some(SINK))
                .find(|s| s.get("description").and_then(|d| d.as_str()) == Some(desc))
                .and_then(index_of);
            match (target, sink_of(&|i| i.get("properties").is_some_and(is_ours))) {
                // The device the panel names has left the machine. Not the same
                // event as a stream ending, and not benign: unplug the
                // headphones the rack says it drives and the sound goes to
                // whatever PipeWire falls back to, while OUTPUT sits there in
                // white still naming them.
                (None, _) => Link::Absent,
                // No output stream of ours on the graph at all — the engine is
                // between devices, or has not started. Nothing to be wrong yet.
                (Some(_), None) => Link::Gone,
                (Some(t), Some(on)) if on == t => Link::Seated,
                (Some(_), Some(on)) => Link::Adrift(put(on)),
            }
        }
    };

    Verdict { routing: Routing { aux, output, howling: false }, successor }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::DESCRIPTION;

    #[test]
    fn the_output_picker_never_offers_our_own_input() {
        assert!(
            !sinks().iter().any(|s| s.name == SINK),
            "our own sink was offered as somewhere to send sound"
        );
    }

    /// The plug, as the guard holds it: Chrome, at the index it lives on.
    fn chrome(index: u32) -> Plug {
        Plug { index, app: "Google Chrome".into() }
    }

    /// A sink, as the answer names it. Indices match the fixtures below.
    fn at(sink: u64, desc: &str) -> Where {
        Where { sink, desc: desc.into() }
    }

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
        assert_eq!(decide(&i, &s, Some(&chrome(88131)), None).routing.aux, Link::Seated);
    }

    #[test]
    fn a_plug_that_was_taken_back_names_who_took_it() {
        let (i, s) = stolen();
        assert_eq!(
            decide(&i, &s, Some(&chrome(88131)), None).routing.aux,
            Link::Adrift(at(47, "EasyEffects Sink"))
        );
    }

    #[test]
    fn an_application_that_stopped_playing_is_gone_rather_than_adrift() {
        let (i, s) = healthy();
        let gone = Plug { index: 9999, app: "mpv".into() };
        let r = decide(&i, &s, Some(&gone), None).routing;
        assert_eq!(r.aux, Link::Gone);
        assert!(!r.aux.astray(), "a finished stream must not raise an alarm");
    }

    #[test]
    fn nothing_plugged_in_is_idle() {
        let (i, s) = healthy();
        assert_eq!(decide(&i, &s, None, None).routing.aux, Link::Idle);
    }

    #[test]
    fn no_aux_sink_means_nothing_can_be_plugged() {
        let (i, mut s) = healthy();
        s.retain(|x| x["name"] != SINK);
        assert_eq!(decide(&i, &s, Some(&chrome(88131)), None).routing.aux, Link::Idle);
    }

    #[test]
    fn an_output_the_rack_really_drives_reads_seated() {
        let (i, s) = healthy();
        assert_eq!(decide(&i, &s, None, Some("Muh Chickin Waffles")).routing.output, Link::Seated);
    }

    #[test]
    fn an_output_that_was_re_homed_says_where_it_went() {
        let (i, s) = stolen();
        assert_eq!(
            decide(&i, &s, None, Some("Muh Chickin Waffles")).routing.output,
            Link::Adrift(at(47, "EasyEffects Sink"))
        );
    }

    #[test]
    fn an_output_device_that_went_away_says_so() {
        let (i, mut s) = healthy();
        s.retain(|x| x["description"] != "Muh Chickin Waffles");
        let out = decide(&i, &s, None, Some("Muh Chickin Waffles")).routing.output;
        assert_eq!(out, Link::Absent);
        assert!(out.astray(), "a device that is not there must not read as fine");
    }

    /// Whereas no output stream of ours at all is the engine between devices,
    #[test]
    fn no_output_stream_of_ours_is_not_an_alarm() {
        let (mut i, s) = healthy();
        i.retain(|x| x["properties"]["node.name"] != "alsa_playback.ten-qd");
        let out = decide(&i, &s, None, Some("Muh Chickin Waffles")).routing.output;
        assert_eq!(out, Link::Gone);
        assert!(!out.astray());
    }

    #[test]
    fn following_the_default_makes_no_claim() {
        let (i, s) = stolen();
        assert_eq!(decide(&i, &s, None, None).routing.output, Link::Idle);
    }

    #[test]
    fn a_name_that_looks_like_the_wanted_description_does_not_count() {
        let (i, mut s) = healthy();
        s.push(serde_json::json!({
            "index": 5150, "name": "Muh Chickin Waffles", "description": "something else"
        }));
        // We are on 87446, whose *description* is the one asked for. Still
        // seated — the impostor's matching name must not steal the comparison.
        assert_eq!(decide(&i, &s, None, Some("Muh Chickin Waffles")).routing.output, Link::Seated);
    }

    #[test]
    fn the_two_questions_are_answered_separately() {
        let (mut i, s) = stolen();
        i[0]["sink"] = serde_json::json!(87671); // Chrome plugged in fine
        let r = decide(&i, &s, Some(&chrome(88131)), Some("Muh Chickin Waffles")).routing;
        assert_eq!(r.aux, Link::Seated);
        assert!(r.output.astray(), "the output is not where the panel says");
    }

    #[test]
    fn a_plug_that_came_out_is_pushed_back_in() {
        let mut r = Reseat::default();
        let (said, push) = r.judge(Link::Adrift(at(87446, "Muh Chickin Waffles")));
        assert!(push, "a first drift is worth one attempt");
        assert_eq!(said, Link::Adrift(at(87446, "Muh Chickin Waffles")));
        // And it held.
        assert_eq!(r.judge(Link::Seated), (Link::Seated, false));
    }

    #[test]
    fn a_stream_something_else_is_holding_is_conceded_after_one_exchange() {
        let mut r = Reseat::default();
        let ee = Link::Adrift(at(47, "EasyEffects Sink"));
        assert!(r.judge(ee.clone()).1, "the first attempt is always owed");
        for round in 0..4 {
            let (said, push) = r.judge(ee.clone());
            assert!(!push, "round {round} tried again — that is the war");
            assert_eq!(said, Link::Contested(at(47, "EasyEffects Sink")));
        }
    }

    #[test]
    fn conceding_to_one_grabber_does_not_forfeit_the_next_recovery() {
        let mut r = Reseat::default();
        r.judge(Link::Adrift(at(47, "EasyEffects Sink")));
        assert_eq!(
            r.judge(Link::Adrift(at(47, "EasyEffects Sink"))).0,
            Link::Contested(at(47, "EasyEffects Sink")),
            "conceded, as it should be"
        );
        // EasyEffects quits; the stream falls to the default output.
        let (said, push) = r.judge(Link::Adrift(at(87446, "Muh Chickin Waffles")));
        assert!(push, "a different sink is a new mishap and is owed an attempt");
        assert_eq!(said, Link::Adrift(at(87446, "Muh Chickin Waffles")));
        assert_eq!(r.judge(Link::Seated), (Link::Seated, false), "and it came home");
    }

    #[test]
    fn a_recycled_index_can_never_make_the_guard_grab_our_own_output() {
        let (inputs, sinks) = healthy();
        // 87605 is ours, sitting on the headphones — exactly what a stale index
        // pointing at a recycled stream would look like. The application is one
        // with nothing playing, so there is no successor to adopt either and
        // the only candidate left is the one that must never be taken.
        let stale = Plug { index: 87605, app: "mpv".into() };
        let r = decide(&inputs, &sinks, Some(&stale), None);
        assert_eq!(r.routing.aux, Link::Gone, "our own stream must read as nothing to watch");
        assert!(!r.routing.aux.astray(), "and must never invite a re-seat");
        assert_eq!(r.successor, None, "and must never be adopted");
    }

    #[test]
    fn a_stream_the_application_replaced_is_adopted() {
        let (mut inputs, sinks) = healthy();
        // Chrome tore down 88131 and came back on a new index, on the default
        // output rather than ours.
        inputs[0]["index"] = serde_json::json!(99001);
        inputs[0]["sink"] = serde_json::json!(87446);

        let r = decide(&inputs, &sinks, Some(&chrome(88131)), None);
        assert_eq!(r.successor, Some(99001), "the replacement is the same plug");
        assert_eq!(
            r.routing.aux,
            Link::Adrift(at(87446, "Muh Chickin Waffles")),
            "and it is judged where it actually is, so it can be seated at once"
        );
    }

    #[test]
    fn a_different_application_is_never_adopted() {
        let (mut inputs, sinks) = healthy();
        inputs[0]["index"] = serde_json::json!(99001);
        inputs[0]["properties"]["application.name"] = serde_json::json!("Spotify");

        let r = decide(&inputs, &sinks, Some(&chrome(88131)), None);
        assert_eq!(r.successor, None, "Spotify is not Chrome");
        assert_eq!(r.routing.aux, Link::Gone);
    }

    // --- the guard's wiring -------------------------------------------------
    //
    // Each half being right does not make the pair right. Everything below is
    // about which decision drives which move, which is invisible to a test that
    // can only see `decide` and `judge` apart.

    #[test]
    fn a_drift_asks_for_the_plugged_stream_to_be_pushed_back() {
        let mut g = Guard::default();
        let raw = Routing { aux: Link::Adrift(at(47, "EasyEffects Sink")), ..Default::default() };
        let (said, acts) = g.tick(Some(Verdict { routing: raw, successor: None }), Some(&chrome(88131)), None);
        assert_eq!(acts, vec![Act::Reseat(88131)], "the plug must actually be pushed");
        assert_eq!(said.unwrap().aux, Link::Adrift(at(47, "EasyEffects Sink")));
    }

    #[test]
    fn each_subject_moves_only_itself() {
        let mut g = Guard::default();
        let raw = Routing {
            aux: Link::Seated,
            output: Link::Adrift(at(47, "EasyEffects Sink")),
            howling: false,
        };
        let (_, acts) = g.tick(Some(Verdict { routing: raw, successor: None }), Some(&chrome(88131)), None);
        assert_eq!(acts, vec![Act::Reroute], "the output drifted; the aux plug did not");
    }

    #[test]
    fn a_graph_that_could_not_be_read_changes_nothing() {
        let mut g = Guard::default();
        let ee = Routing { aux: Link::Adrift(at(47, "EasyEffects Sink")), ..Default::default() };
        g.tick(Some(Verdict { routing: ee.clone(), successor: None }), Some(&chrome(88131)), None); // one attempt, spent
        assert_eq!(g.tick(None, Some(&chrome(88131)), None), (None, Vec::new()), "nothing seen, nothing done");
        let (said, acts) = g.tick(Some(Verdict { routing: ee, successor: None }), Some(&chrome(88131)), None);
        assert!(acts.is_empty(), "the failed read handed the attempt back");
        assert_eq!(said.unwrap().aux, Link::Contested(at(47, "EasyEffects Sink")));
    }

    #[test]
    fn a_different_stream_is_owed_its_own_attempt() {
        let mut g = Guard::default();
        let ee = Routing { aux: Link::Adrift(at(47, "EasyEffects Sink")), ..Default::default() };
        g.tick(Some(Verdict { routing: ee.clone(), successor: None }), Some(&chrome(88131)), None);
        let (said, acts) = g.tick(Some(Verdict { routing: ee.clone(), successor: None }), Some(&chrome(88131)), None);
        assert!(acts.is_empty(), "same plug, same sink — conceded");
        assert_eq!(said.unwrap().aux, Link::Contested(at(47, "EasyEffects Sink")));

        // The operator plugs in a different application.
        let (said, acts) = g.tick(Some(Verdict { routing: ee, successor: None }), Some(&chrome(99999)), None);
        assert_eq!(acts, vec![Act::Reseat(99999)]);
        assert_eq!(said.unwrap().aux, Link::Adrift(at(47, "EasyEffects Sink")));
    }

    #[test]
    fn a_stream_that_ended_is_dropped_by_name_not_blindly() {
        let mut g = Guard::default();
        let raw = Routing { aux: Link::Gone, ..Default::default() };
        let (_, acts) = g.tick(Some(Verdict { routing: raw, successor: None }), Some(&chrome(88131)), None);
        assert!(
            acts.contains(&Act::Unwatch(88131)),
            "the index it judged has to travel with the instruction: {acts:?}"
        );
    }

    #[test]
    fn a_different_output_device_is_owed_its_own_attempt() {
        let mut g = Guard::default();
        let ee = Routing { output: Link::Adrift(at(47, "EasyEffects Sink")), ..Default::default() };
        g.tick(Some(Verdict { routing: ee.clone(), successor: None }), None, Some("Muh Chickin Waffles"));
        let (said, acts) = g.tick(Some(Verdict { routing: ee.clone(), successor: None }), None, Some("Muh Chickin Waffles"));
        assert!(acts.is_empty(), "same target, same grabber — conceded");
        assert_eq!(said.unwrap().output, Link::Contested(at(47, "EasyEffects Sink")));

        // The operator picks a different output.
        let (said, acts) = g.tick(Some(Verdict { routing: ee, successor: None }), None, Some("USB Audio Speakers"));
        assert_eq!(acts, vec![Act::Reroute], "the new device is owed an attempt");
        assert_eq!(said.unwrap().output, Link::Adrift(at(47, "EasyEffects Sink")));
    }

    #[test]
    fn two_sinks_that_share_a_name_are_still_two_places() {
        let mut g = Guard::default();
        let first = Routing { aux: Link::Adrift(at(47, "Digital Output")), ..Default::default() };
        let second = Routing { aux: Link::Adrift(at(48, "Digital Output")), ..Default::default() };
        g.tick(Some(Verdict { routing: first.clone(), successor: None }), Some(&chrome(88131)), None);
        let (_, acts) = g.tick(Some(Verdict { routing: first, successor: None }), Some(&chrome(88131)), None);
        assert!(acts.is_empty(), "same sink — conceded");
        let (_, acts) = g.tick(Some(Verdict { routing: second, successor: None }), Some(&chrome(88131)), None);
        assert_eq!(acts, vec![Act::Reseat(88131)], "a different sink with the same name");
    }

    #[test]
    fn a_grabber_that_pauses_for_breath_is_still_only_fought_once() {
        let mut r = Reseat::default();
        let ee = Link::Adrift(at(47, "EasyEffects Sink"));
        assert!(r.judge(ee.clone()).1, "the first attempt is always owed");

        let mut attempts = 0;
        for _ in 0..20 {
            // It holds for a couple of ticks, then is taken again.
            r.judge(Link::Seated);
            r.judge(Link::Seated);
            let (said, push) = r.judge(ee.clone());
            if push {
                attempts += 1;
            }
            assert_eq!(said, Link::Contested(at(47, "EasyEffects Sink")));
        }
        assert_eq!(attempts, 0, "traded {attempts} further moves — that is the slow war");
    }

    #[test]
    fn an_attempt_is_returned_once_the_plug_has_really_held() {
        let mut r = Reseat::default();
        let ee = Link::Adrift(at(47, "EasyEffects Sink"));
        r.judge(ee.clone());
        for _ in 0..SETTLED {
            r.judge(Link::Seated);
        }
        assert!(r.judge(ee).1, "after settling, the same sink is owed a fresh attempt");
    }

    #[test]
    fn a_repeat_grabber_is_given_up_on_by_degrees() {
        let mut r = Reseat::default();
        let ee = || Link::Adrift(at(47, "EasyEffects Sink"));

        let mut waits = Vec::new();
        for exchange in 0..5 {
            assert!(r.judge(ee()).1, "exchange {exchange} must begin with an attempt");
            // How long the plug must now sit still before another is owed.
            let mut ticks = 0;
            while r.spent_on.is_some() {
                ticks += 1;
                r.judge(Link::Seated);
            }
            waits.push(ticks);
        }

        assert_eq!(waits, vec![30, 60, 120, 240, 240], "{waits:?}");
    }

    #[test]
    fn a_stream_that_ends_spends_no_attempt() {
        let mut r = Reseat::default();
        r.judge(Link::Adrift(at(47, "EasyEffects Sink")));
        r.judge(Link::Gone);
        assert!(
            r.judge(Link::Adrift(at(47, "EasyEffects Sink"))).1,
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
}
