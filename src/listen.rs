//! The listening log.
//!
//! TRACK mode does not record audio. It writes down what played, in what
//! order, and where it came from — which is what a mixtape always actually
//! was. Nobody compiling one in 1986 thought they were manufacturing music;
//! they were recording an *order*. The audio was the storage medium it had to
//! be written on, and it no longer has to be.
//!
//! ## It is a log, not a tape
//!
//! The obvious shape is "press REC, get a tape". The shape here is a running
//! record of everything the rack heard, and a tape is something you cut out of
//! it afterwards. Play YouTube Music in a tab in the morning and Spotify in
//! the afternoon, and what you want at the end of the week is not two tapes —
//! it is one list of the things you liked, gathered across services that have
//! no interest in letting you hold that list.
//!
//! ## Append-only, and why that is load-bearing
//!
//! `$XDG_STATE_HOME/ten-qd/listening.jsonl`, one JSON object per line,
//! appended and never rewritten. Appending a line is a single write that
//! cannot corrupt what came before, a torn final line after a crash is
//! discardable on its own, and the whole thing stays greppable. A TOML or XML
//! log would have to be re-serialised on every track change — a rewrite of the
//! entire history, several times an hour, for a program that should survive
//! being killed.
//!
//! ## Honesty
//!
//! The log records what it observed and nothing else. A duration is how long
//! you actually played something, not how long the track is; a player that
//! reports no title gets no entry invented for it; and nothing here decides
//! what you are allowed to listen to. The deck's job is to tell the truth
//! about the signal.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::mpris::NowPlaying;

/// Below this, a track was skipped past rather than played. The log keeps
/// every play — how often you played something is the signal — but scrubbing
/// through an album at four seconds a track is not twelve plays.
const MIN_PLAY: Duration = Duration::from_secs(5);

/// The most one poll may credit.
///
/// Time is credited in arrears — whatever ran since the last look ran under the
/// metadata we already held — which quietly assumes the looks keep coming. They
/// do not: suspend the machine at 22:00 with Spotify playing and the next poll
/// arrives ten hours later, holding a `playing` flag from last night. Without a
/// ceiling that becomes a ten-hour play, written once into a file that is never
/// rewritten and carried into the `#EXTINF` of every tape cut from that month.
///
/// A gap this long is the machine not looking, not you listening. Comfortably
/// above the 500 ms bus poll and the 33 ms frame, so ordinary scheduling jitter
/// still counts in full. A forward clock step and a long stall are the same
/// event from here and get the same treatment.
const MAX_CREDIT: Duration = Duration::from_secs(5);

/// How long a player may say nothing before its open entry is closed.
///
/// A failed metadata call is momentary and holding through it keeps one play
/// whole. A player that clears its metadata and stays cleared is a different
/// thing, and holding for that would credit its time to whichever track was
/// named last. This is where the code stops calling a silence momentary.
const BLANK_GRACE: Duration = Duration::from_secs(15);

/// One line of the log: something that played, and for how long.
///
/// Read back as well as written, and the file is hand-editable — so every
/// field defaults rather than being required. A line missing something is a
/// line with a gap in it, not a reason to abandon the rest of the history.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Entry {
    /// When it started, RFC 3339 in UTC. A local timestamp would reorder
    /// itself twice a year and lie about which side of a flight it was on.
    pub at: String,
    /// Which run of the rack this belongs to, so "that Tuesday evening" is a
    /// thing you can name and select later.
    pub session: String,
    /// The player's own name for itself: "Spotify", "Chrome", "mpv".
    pub player: String,
    /// Written even when empty, unlike the two fields below. A browser tab
    /// routinely reports a title and nothing else — "Playback | Omniguard" —
    /// and a projection that has to cope with a missing artist on some lines
    /// and an empty one on others is being asked the same question twice.
    pub artist: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub album: String,
    /// Seconds actually played. Skip halfway through and this says so.
    pub seconds: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub uri: String,
}

/// Format a `SystemTime` as `2026-07-26T21:55:56Z`.
///
/// Hand-rolled rather than pulled in as a dependency: this is the only date
/// the program ever formats, and it does not need a calendar library to write
/// one line. Anything before 1970 stamps as the epoch — the log records
/// listening, and the clock being that wrong is not something to encode.
pub fn stamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days since the epoch to a civil date. Howard Hinnant's `civil_from_days`,
/// which is exact for the whole proleptic Gregorian calendar and shorter than
/// the table-of-month-lengths version it replaces.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe as i64 + era * 400 + i64::from(m <= 2), m, d)
}

/// A session name derived from when the rack was switched on. Sortable,
/// filename-safe, and readable — `20260726-215556`.
pub fn session_id(t: SystemTime) -> String {
    stamp(t).chars().filter(|c| c.is_ascii_digit() || *c == 'T').collect::<String>().replace('T', "-")
}

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/// A player we are currently following, and how much of it has run.
#[derive(Clone, Debug)]
struct Open {
    /// The bus name, which is stable across metadata changes where the
    /// player's own identity string is not always distinct — two Chromium
    /// windows are both "Chromium".
    bus: String,
    entry: Entry,
    played: Duration,
    /// The last poll, so time can be credited in arrears.
    seen: SystemTime,
    playing: bool,
    /// When this player last went quiet about what it is playing, if it has.
    /// Cleared the moment it says something again.
    blank_since: Option<SystemTime>,
}

impl Open {
    fn advance(&mut self, now: SystemTime) {
        if self.playing {
            let gap = now.duration_since(self.seen).unwrap_or(Duration::ZERO);
            self.played += gap.min(MAX_CREDIT);
        }
        self.seen = now;
    }

    /// Whether this has run long enough to be a play rather than a skip.
    ///
    /// The one predicate. Both the panel's count and the decision to write ask
    /// it, because a readout that used a different rule from the writer would
    /// be claiming pending entries that never arrive.
    fn counts(&self) -> bool {
        self.played >= MIN_PLAY
    }

    /// The finished entry, if it ran long enough to have been a play.
    fn finish(&self) -> Option<Entry> {
        self.counts().then(|| Entry { seconds: self.played.as_secs(), ..self.entry.clone() })
    }
}

/// Turns a sequence of "here is every player on the bus right now" snapshots
/// into log entries.
///
/// Deliberately clock-free: every method takes the time as an argument rather
/// than reading it, so the whole of the interesting behaviour — track
/// boundaries, players coming and going, pauses — is testable without waiting
/// for real seconds to pass.
pub struct Watcher {
    session: String,
    open: Vec<Open>,
}

impl Watcher {
    pub fn new(session: String) -> Self {
        Watcher { session, open: Vec::new() }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    /// What is being followed right now — one line per player whose entry
    /// would be written if everything stopped at this instant.
    ///
    /// Filtered by the same `counts()` the writer uses, not by "has an entry
    /// open". The difference is not cosmetic: pause two seconds into a track
    /// and walk away, and the unfiltered version leaves the panel claiming one
    /// pending entry for the rest of the session, for a play that is never
    /// written. A new track therefore takes five seconds to appear here, which
    /// is the honest cost of the count meaning what it says.
    pub fn following(&self) -> impl Iterator<Item = (&str, &str)> {
        self.open
            .iter()
            .filter(|o| o.counts())
            .map(|o| (o.entry.player.as_str(), o.entry.title.as_str()))
    }

    /// Fold one poll of the bus into the log, returning whatever just closed.
    pub fn observe(&mut self, seen: &[NowPlaying], now: SystemTime) -> Vec<Entry> {
        let mut done = Vec::new();

        // Time first. Whatever ran since the last poll ran under the metadata
        // we already hold, so it has to be credited before anything is
        // allowed to change underneath it.
        for o in &mut self.open {
            o.advance(now);
        }

        // A player that vanished and a player that moved on to something else
        // are the same event from the log's side: what it had open is over.
        self.open.retain_mut(|o| match seen.iter().find(|s| s.bus == o.bus) {
            Some(s) if same_track(s, &o.entry) => {
                o.playing = s.playing;
                o.blank_since = None;
                true
            }
            // A player still there but saying nothing is not a track change.
            // `snapshot()` reports every field empty from a single failed
            // metadata call, so without this a D-Bus hiccup halfway through a
            // 200-second track splits it into two hundred-second plays,
            // doubling its play count and halving the length every tape cut
            // from it inherits. `blank` already refuses to *open* an entry on
            // no information; refusing to close one on the same evidence is
            // the other half of that.
            //
            // Two things this must not do. It must not keep the old `playing`
            // flag — a player that clears its metadata at the moment it stops
            // would then accrue at real time for as long as the terminal
            // stayed open, writing an hour-long play of whatever was last
            // named. And it must not hold for ever: a hiccup is momentary by
            // definition, and past the grace period the honest reading is that
            // whatever is on the other end is no longer the track we opened.
            Some(s) if blank(s) => {
                o.playing = s.playing;
                if o.blank_since.is_none() {
                    o.blank_since = Some(now);
                }
                let waited = o
                    .blank_since
                    .and_then(|t| now.duration_since(t).ok())
                    .unwrap_or(Duration::ZERO);
                if waited > BLANK_GRACE {
                    done.extend(o.finish());
                    return false;
                }
                true
            }
            _ => {
                done.extend(o.finish());
                false
            }
        });

        // Anything playing that we are not already following starts an entry.
        // Paused is not followed: an entry begins when something begins, and a
        // player sitting paused has played nothing yet.
        for s in seen {
            let known = self.open.iter().any(|o| o.bus == s.bus);
            if !known && s.playing && !blank(s) {
                let o = Open {
                    bus: s.bus.clone(),
                    entry: Entry {
                        at: stamp(now),
                        session: self.session.clone(),
                        player: s.player.clone(),
                        artist: s.artist.clone(),
                        title: s.title.clone(),
                        album: s.album.clone(),
                        seconds: 0,
                        uri: s.uri.clone(),
                    },
                    played: Duration::ZERO,
                    seen: now,
                    playing: true,
                    blank_since: None,
                };
                self.open.push(o);
            }
        }

        done
    }

    /// The rack is being switched off. Everything still running closes here,
    /// with however long it actually ran — a session that ends mid-track
    /// should still record the part you heard.
    pub fn close(&mut self, _now: SystemTime) -> Vec<Entry> {
        // Deliberately does not credit the time since the last poll. On a
        // clean exit that is one frame and worth nothing; after an outage or a
        // suspend it is time nobody observed, and crediting it could push an
        // entry the panel had just reported as *not* pending over the
        // five-second line and write it. What was observed is what is written.
        let done: Vec<Entry> = self.open.iter().filter_map(Open::finish).collect();
        self.open.clear();
        done
    }
}

/// Nothing to write down. A player reporting no artist, title or album is
/// playing *something*, but nothing anyone could put on a list.
fn blank(s: &NowPlaying) -> bool {
    s.title.is_empty() && s.artist.is_empty() && s.album.is_empty()
}

/// The track boundary. MPRIS gives no event for "the next song started", so
/// the tuple changing is the only signal there is.
fn same_track(s: &NowPlaying, e: &Entry) -> bool {
    s.title == e.title && s.artist == e.artist && s.album == e.album
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/// The log file, opened afresh for each append.
///
/// Holding the handle open would be faster and would also mean a crash could
/// leave a buffered line unwritten. Entries arrive minutes apart; the open is
/// free at that rate, and every line that returns `Ok` is on disk.
pub struct Log {
    path: PathBuf,
}

impl Log {
    /// Where the log lives, alongside the 12-volt memory.
    pub fn path() -> Option<PathBuf> {
        Some(crate::memory::state_dir()?.join("listening.jsonl"))
    }

    pub fn at(path: PathBuf) -> Self {
        Log { path }
    }

    pub fn file(&self) -> &Path {
        &self.path
    }

    /// Append one entry. The caller reports failures rather than this
    /// retrying: a log that silently stops writing is worse than one that
    /// says on the panel that it cannot.
    pub fn append(&self, e: &Entry) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut line = serde_json::to_string(e).map_err(std::io::Error::other)?;
        line.push('\n');

        let mut f =
            std::fs::OpenOptions::new().create(true).read(true).append(true).open(&self.path)?;
        // A crash mid-write leaves a line with no newline on the end.
        // Appending straight onto it would fuse the wreck to the next entry
        // and cost two plays instead of one, so the broken line is closed
        // first — which is what makes "a torn final line is discardable on its
        // own" true rather than merely intended.
        let len = f.metadata()?.len();
        if len > 0 {
            let mut last = [0u8; 1];
            f.seek(std::io::SeekFrom::Start(len - 1))?;
            f.read_exact(&mut last)?;
            if last[0] != b'\n' {
                f.write_all(b"\n")?;
            }
        }
        f.write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Milliseconds since the epoch, so a test can poll at the rate the panel
    /// actually polls at rather than in improbable one-shot jumps.
    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    /// Look at the bus every 500 ms from `from` to `to`, the way the loop does.
    ///
    /// Time is credited per poll and capped per poll, so the number of looks is
    /// part of the arithmetic — a test that jumped straight from 0 to 200 s
    /// would be measuring something the program never does.
    fn ticks(w: &mut Watcher, seen: &[NowPlaying], from: u64, to: u64) -> Vec<Entry> {
        let mut done = Vec::new();
        let mut t = from;
        while t <= to {
            done.extend(w.observe(seen, at(t)));
            t += 500;
        }
        done
    }

    fn np(bus: &str, title: &str, playing: bool) -> NowPlaying {
        NowPlaying {
            bus: bus.into(),
            player: bus.into(),
            title: title.into(),
            artist: "Boards of Canada".into(),
            album: "Tomorrow's Harvest".into(),
            uri: String::new(),
            playing,
        }
    }

    /// A snapshot of a player that is there but saying nothing — what a single
    /// failed `get_metadata()` produces. `playing` matters: a player that
    /// clears its metadata *because it stopped* reports false, and that is the
    /// case that used to accrue time for ever.
    fn mute(bus: &str, playing: bool) -> NowPlaying {
        NowPlaying { bus: bus.into(), player: bus.into(), playing, ..Default::default() }
    }

    #[test]
    fn a_timestamp_is_rfc3339_in_utc() {
        assert_eq!(stamp(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(stamp(at(1_774_562_156_000)), "2026-03-26T21:55:56Z");
    }

    #[test]
    fn leap_days_land_where_they_should() {
        // 2024-02-29, a leap year; and 2100-03-01, which is not one.
        assert_eq!(stamp(at(1_709_164_800_000)), "2024-02-29T00:00:00Z");
        assert_eq!(stamp(at(4_107_542_400_000)), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn a_session_is_named_for_when_it_started() {
        assert_eq!(session_id(at(1_774_562_156_000)), "20260326-215556");
    }

    #[test]
    fn a_track_closes_when_the_metadata_changes() {
        let mut w = Watcher::new("s".into());
        assert!(ticks(&mut w, &[np("spotify", "Reach for the Dead", true)], 0, 199_500).is_empty());
        let done = w.observe(&[np("spotify", "Cold Earth", true)], at(200_000));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].title, "Reach for the Dead");
        assert_eq!(done[0].seconds, 200, "the duration is what was played");
        assert_eq!(done[0].at, "1970-01-01T00:00:00Z", "stamped when it started");
    }

    #[test]
    fn a_player_leaving_closes_what_it_had_open() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "Cold Earth", true)], 0, 59_500);
        let done = w.observe(&[], at(60_000));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].seconds, 60);
    }

    #[test]
    fn a_skip_is_not_a_play() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "a", true)], 0, 1_500);
        let done = w.observe(&[np("spotify", "b", true)], at(2_000));
        assert!(done.is_empty(), "two seconds is scrubbing, not listening");
    }

    #[test]
    fn paused_time_is_not_played_time() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "a", true)], 0, 10_000);
        ticks(&mut w, &[np("spotify", "a", false)], 10_500, 600_000);
        ticks(&mut w, &[np("spotify", "a", true)], 600_500, 610_000);
        let done = w.close(at(610_500));
        assert_eq!(done[0].seconds, 20, "ten seconds either side of a long pause");
    }

    /// The one that corrupts the archive rather than one line of it.
    ///
    /// Time is credited in arrears using the flag from the *previous* poll,
    /// which assumes the polls keep coming. Suspend the machine and they stop:
    /// the next look holds a `playing` from last night and a gap of hours.
    #[test]
    fn a_suspended_machine_is_not_ten_hours_of_listening() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "Cold Earth", true)], 0, 30_000);
        // The lid closes. Ten hours later the first poll arrives, still
        // holding last night's flag, and the player has since paused.
        w.observe(&[np("spotify", "Cold Earth", false)], at(36_030_000));
        let done = w.close(at(36_030_500));
        assert_eq!(
            done[0].seconds, 35,
            "credited across a suspend, into a file that is never rewritten"
        );
    }

    #[test]
    fn a_paused_player_starts_nothing() {
        let mut w = Watcher::new("s".into());
        w.observe(&[np("spotify", "a", false)], at(0));
        assert_eq!(w.following().count(), 0);
        ticks(&mut w, &[np("spotify", "a", true)], 500, 10_000);
        assert_eq!(w.following().count(), 1);
    }

    /// `following` is a claim about what would be written. It used to count
    /// every open entry, including those too short to be written at all —
    /// so pausing two seconds into a track left the panel reporting a pending
    /// entry for the rest of the session, for a play that never arrived.
    #[test]
    fn the_panel_only_counts_what_would_actually_be_written() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "a", true)], 0, 2_000);
        assert_eq!(w.following().count(), 0, "two seconds in, nothing would be written");

        // Pause there and walk away. The entry stays open and stays unwritable.
        ticks(&mut w, &[np("spotify", "a", false)], 2_500, 3_600_000);
        assert_eq!(w.following().count(), 0, "and the panel must not keep claiming it");
        assert!(w.close(at(3_600_500)).is_empty(), "nothing was ever going to be written");
    }

    /// A single failed `get_metadata()` reports every field empty. Treating
    /// that as a track change split one play into two — doubling its play
    /// count and halving the length every tape cut from it inherits.
    #[test]
    fn a_momentary_silence_from_a_player_is_not_a_new_track() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "Cold Earth", true)], 0, 100_000);
        let done = ticks(&mut w, &[mute("spotify", true)], 100_500, 101_000);
        assert!(done.is_empty(), "a hiccup is not the end of a track");

        ticks(&mut w, &[np("spotify", "Cold Earth", true)], 101_500, 200_000);
        let done = w.close(at(200_500));
        assert_eq!(done.len(), 1, "one play, not two");
        assert_eq!(done[0].seconds, 200);
    }

    /// The hold added for the hiccup above must not become a licence to accrue
    /// time on a stale flag. mpv reaching the end of its playlist keeps the bus
    /// object, sets Stopped and clears the metadata — and the entry used to go
    /// on counting at real time for as long as the terminal stayed open,
    /// writing an hour-long play of whatever was named last.
    #[test]
    fn a_player_that_goes_quiet_because_it_stopped_accrues_nothing() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("mpv", "Cold Earth", true)], 0, 100_000);
        // Blank *and* stopped, well inside the grace period so the hold is
        // still in force — this measures the flag, not the timeout. Holding
        // the old `playing` here would bank ten seconds nobody listened to.
        ticks(&mut w, &[mute("mpv", false)], 100_500, 110_000);
        let done = w.close(at(110_000));
        assert_eq!(
            done[0].seconds, 100,
            "credited a player that had stopped, on a flag from before it stopped"
        );
    }

    /// …and the hold is bounded even while the player still claims to play,
    /// because past the grace period nothing supports the claim that what is
    /// running is still the track we opened.
    #[test]
    fn a_silence_that_is_not_momentary_closes_the_entry() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("mpv", "Cold Earth", true)], 0, 100_000);
        let done = ticks(&mut w, &[mute("mpv", true)], 100_500, 200_000);
        assert_eq!(done.len(), 1, "a silence this long is not a hiccup");
        assert!(done[0].seconds <= 116, "got {}s", done[0].seconds);
        assert_eq!(w.following().count(), 0);
    }

    #[test]
    fn two_players_are_logged_side_by_side() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "a", true), np("chromium", "b", true)], 0, 100_000);
        assert_eq!(w.following().count(), 2);
        let done = w.observe(&[np("spotify", "a", true)], at(100_500));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].player, "chromium");
    }

    #[test]
    fn a_player_with_nothing_to_say_gets_no_entry() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[mute("mpv", true)], 0, 10_000);
        assert_eq!(w.following().count(), 0);
    }

    #[test]
    fn closing_the_session_writes_what_was_still_running() {
        let mut w = Watcher::new("s".into());
        ticks(&mut w, &[np("spotify", "a", true)], 0, 90_000);
        let done = w.close(at(90_000));
        assert_eq!(done[0].seconds, 90);
        assert_eq!(w.following().count(), 0);
        assert!(w.close(at(100_000)).is_empty(), "closing twice must not duplicate");
    }

    /// Append-only survives a crash only if the wreckage stays on one line.
    #[test]
    fn a_line_left_unterminated_by_a_crash_does_not_swallow_the_next_one() {
        let dir = std::env::temp_dir().join(format!("ten-qd-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let log = Log::at(dir.join("listening.jsonl"));
        std::fs::write(log.file(), r#"{"at":"2026-07-26T10:00:0"#).expect("half a line");

        log.append(&Entry { title: "survivor".into(), ..Default::default() }).expect("append");
        let text = std::fs::read_to_string(log.file()).expect("read back");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(text.lines().count(), 2, "the wreck must not fuse to the entry: {text}");
        assert!(text.lines().nth(1).unwrap().contains("survivor"), "{text}");
    }

    #[test]
    fn entries_serialise_one_per_line_and_omit_what_is_empty() {
        let dir = std::env::temp_dir().join(format!("ten-qd-log-{}", std::process::id()));
        let log = Log::at(dir.join("listening.jsonl"));
        let e = Entry {
            at: "2026-07-26T21:55:56Z".into(),
            session: "20260726-215556".into(),
            player: "Spotify".into(),
            artist: "Boards of Canada".into(),
            title: "Cold Earth".into(),
            album: String::new(),
            seconds: 214,
            uri: String::new(),
        };
        log.append(&e).expect("append");
        log.append(&e).expect("append twice");
        let text = std::fs::read_to_string(log.file()).expect("read back");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(text.lines().count(), 2, "one object per line, appended");
        assert!(!text.contains("album"), "an empty field is left out, not written blank");
        assert!(text.starts_with(r#"{"at":"2026-07-26T21:55:56Z","session":"#));
        assert!(text.lines().next().unwrap().ends_with(r#""seconds":214}"#));
    }
}
