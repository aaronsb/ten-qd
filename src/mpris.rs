//! Reaching over to hit next on the Discman.
//!
//! A real auxiliary input was a one-way cable. Its keys — if it had any — did
//! nothing to the thing on the other end; you reached over to the passenger
//! seat and pressed the button yourself. This is the one place the build
//! improves on the object it is imitating, and it improves on it using the
//! interface that already exists rather than one of ours.
//!
//! MPRIS2 is the desktop standard for exactly this, and every service the
//! adapter is likely to be carrying already speaks it: the Spotify client,
//! Chromium (so YouTube Music, Apple Music and Pandora in a tab), Firefox,
//! mpv, and most native players. One protocol, every source.
//!
//! Polling happens on its own thread at a couple of hertz. D-Bus calls block,
//! and blocking the render loop to ask Spotify what it is playing would be a
//! poor trade for a readout that changes every few minutes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the player last told us. Cloned out by the UI each frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NowPlaying {
    /// The D-Bus name, which is the only stable identity here: two Chromium
    /// windows both call themselves "Chromium", and the listening log has to
    /// tell them apart to know whether a track changed or a second one
    /// started.
    pub bus: String,
    /// The player's own name for itself: "Spotify", "Chromium", "mpv".
    pub player: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Where the track lives, as the player understands it — a `spotify:` or
    /// `https:` URI for a service, a `file://` URL for a local file. Empty
    /// when the player does not say.
    pub uri: String,
    pub playing: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Transport {
    PlayPause,
    Stop,
    Next,
    Previous,
}

pub struct Mpris {
    now: Arc<Mutex<Option<NowPlaying>>>,
    /// Every player on the bus, as the last poll saw it. The aux bay wants one
    /// player — the one on the cable — and the listening log wants all of
    /// them, so the poll gathers everything and `now` is a choice made from it.
    ///
    /// `None` means the bus could not be read at all, which is a different
    /// fact from "nobody is playing" and has to stay different: an empty list
    /// tells the listening log every player has gone away, and answering a
    /// D-Bus outage with that would close every open entry and start fresh
    /// ones when it cleared.
    all: Arc<Mutex<Option<Vec<NowPlaying>>>>,
    /// The application name to prefer when several players are running —
    /// normally whatever is plugged into the aux input.
    prefer: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Mpris {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Mpris {
    /// Start watching. Never fails: a machine with no D-Bus, or no player
    /// running, simply reports nothing and the deck falls back to being the
    /// one-way cable it is imitating.
    pub fn start() -> Self {
        let now: Arc<Mutex<Option<NowPlaying>>> = Arc::new(Mutex::new(None));
        let all: Arc<Mutex<Option<Vec<NowPlaying>>>> = Arc::new(Mutex::new(None));
        let prefer: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));

        let (t_now, t_all, t_prefer, t_stop) = (now.clone(), all.clone(), prefer.clone(), stop.clone());
        std::thread::Builder::new()
            .name("ten-qd/mpris".into())
            .spawn(move || {
                while !t_stop.load(Ordering::Relaxed) {
                    let want = t_prefer.lock().ok().and_then(|g| g.clone());
                    let found = poll();
                    let chosen =
                        found.as_ref().and_then(|f| pick(f, want.as_deref())).cloned();
                    if let Ok(mut g) = t_now.lock() {
                        *g = chosen;
                    }
                    if let Ok(mut g) = t_all.lock() {
                        *g = found;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .ok();

        Mpris { now, all, prefer, stop }
    }

    /// Tell the watcher which player to prefer. Several can be running at
    /// once — a browser and Spotify — and the right one is whichever is
    /// plugged into the aux input.
    pub fn prefer(&self, app: Option<String>) {
        if let Ok(mut g) = self.prefer.lock() {
            *g = app;
        }
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        self.now.lock().ok().and_then(|g| g.clone())
    }

    /// Every player the last poll found, playing or not. This is what the
    /// listening log watches: Chromium at breakfast, Spotify after lunch, both
    /// at once while one is paused.
    ///
    /// `None` until the first successful scan, and again whenever one fails —
    /// "the bus did not answer", which callers must not read as "nothing is
    /// playing".
    pub fn all(&self) -> Option<Vec<NowPlaying>> {
        self.all.lock().ok().and_then(|g| g.clone())
    }

    /// Send a transport command. Synchronous, but a single D-Bus round trip
    /// on a keypress is imperceptible; it is *polling* that had to move off
    /// the render loop, not one-shot commands.
    pub fn send(&self, t: Transport) -> bool {
        let want = self.prefer.lock().ok().and_then(|g| g.clone());
        let Some(player) = find(want.as_deref()) else { return false };
        let r = match t {
            Transport::PlayPause => player.play_pause(),
            Transport::Stop => player.stop(),
            Transport::Next => player.next(),
            Transport::Previous => player.previous(),
        };
        r.is_ok()
    }
}

/// Whether a player answers to a name the operator asked for.
///
/// Loose on purpose: PipeWire calls it "Spotify" and so does MPRIS, but a
/// browser is "Chromium" on one side and "chromium.instance3976392" on the
/// other.
fn prefers(identity: &str, bus: &str, want: &str) -> bool {
    let (id, bus, w) = (
        identity.to_ascii_lowercase(),
        bus.to_ascii_lowercase(),
        want.to_ascii_lowercase(),
    );
    (!id.is_empty() && (id.contains(&w) || w.contains(&id))) || bus.contains(&w)
}

/// Choose one snapshot out of the poll: the one that answers to `want`, else
/// whichever is actually playing, since a player merely sitting paused is not
/// what anyone means by "what is on".
fn pick<'a>(players: &'a [NowPlaying], want: Option<&str>) -> Option<&'a NowPlaying> {
    if let Some(w) = want
        && let Some(p) = players.iter().find(|p| prefers(&p.player, &p.bus, w))
    {
        return Some(p);
    }
    players.iter().find(|p| p.playing).or_else(|| players.first())
}

/// Pick a live player to send a command to. Separate from `pick` because a
/// snapshot cannot be told to skip a track — this re-scans the bus, which is
/// fine for a keypress and would not be for a 2 Hz poll.
fn find(want: Option<&str>) -> Option<mpris::Player> {
    let finder = mpris::PlayerFinder::new().ok()?;
    let players = finder.find_all().ok()?;

    if let Some(w) = want
        && let Some(i) = players.iter().position(|p| prefers(p.identity(), p.bus_name(), w))
    {
        return players.into_iter().nth(i);
    }

    let playing = players
        .iter()
        .position(|p| matches!(p.get_playback_status(), Ok(mpris::PlaybackStatus::Playing)));
    match playing {
        Some(i) => players.into_iter().nth(i),
        None => players.into_iter().next(),
    }
}

/// Snapshot every player on the bus. One scan serves both readers.
///
/// `None` when the bus itself could not be reached — no D-Bus, or a failed
/// enumeration. An empty `Some` is the other thing entirely: the bus answered
/// and nobody is running. Collapsing the two is what would let a momentary
/// outage read as every player quitting at once.
fn poll() -> Option<Vec<NowPlaying>> {
    let finder = mpris::PlayerFinder::new().ok()?;
    let players = finder.find_all().ok()?;
    Some(players.iter().map(snapshot).collect())
}

fn snapshot(player: &mpris::Player) -> NowPlaying {
    let meta = player.get_metadata().ok();
    NowPlaying {
        bus: player.bus_name().to_string(),
        player: player.identity().to_string(),
        playing: matches!(player.get_playback_status(), Ok(mpris::PlaybackStatus::Playing)),
        title: meta.as_ref().and_then(|m| m.title()).unwrap_or_default().to_string(),
        artist: meta
            .as_ref()
            .and_then(|m| m.artists())
            .map(|a| a.join(", "))
            .unwrap_or_default(),
        album: meta.as_ref().and_then(|m| m.album_name()).unwrap_or_default().to_string(),
        uri: meta.as_ref().and_then(|m| m.url()).unwrap_or_default().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(bus: &str, identity: &str, playing: bool) -> NowPlaying {
        NowPlaying { bus: bus.into(), player: identity.into(), playing, ..Default::default() }
    }

    /// Exercises the real bus when one is present. Read-only, so it is safe
    /// anywhere; a machine with no D-Bus or no player reports an empty list
    /// rather than failing.
    #[test]
    fn polling_either_finds_players_or_reports_none() {
        for n in poll().unwrap_or_default() {
            assert!(!n.bus.is_empty(), "a found player must have a bus name");
        }
    }

    #[test]
    fn a_preferred_player_wins_over_one_that_is_playing() {
        let all = [
            player("org.mpris.MediaPlayer2.spotify", "Spotify", true),
            player("org.mpris.MediaPlayer2.chromium.instance3976392", "Chromium", false),
        ];
        assert_eq!(pick(&all, Some("Chromium")).unwrap().player, "Chromium");
    }

    #[test]
    fn without_a_preference_whatever_is_playing_wins() {
        let all = [
            player("org.mpris.MediaPlayer2.chromium.instance1", "Chromium", false),
            player("org.mpris.MediaPlayer2.spotify", "Spotify", true),
        ];
        assert_eq!(pick(&all, None).unwrap().player, "Spotify");
        assert!(pick(&[], None).is_none());
    }

    #[test]
    fn an_unmatched_preference_falls_back_rather_than_reporting_nothing() {
        let all = [player("org.mpris.MediaPlayer2.spotify", "Spotify", true)];
        assert_eq!(pick(&all, Some("Rhythmbox")).unwrap().player, "Spotify");
    }

    #[test]
    fn a_player_is_matched_by_bus_name_when_its_identity_does_not_say() {
        // PipeWire labels the stream "chromium"; MPRIS calls the player
        // "Chromium" and buses it as "chromium.instance3976392".
        assert!(prefers("Chromium", "org.mpris.MediaPlayer2.chromium.instance1", "chromium"));
        assert!(prefers("Spotify", "org.mpris.MediaPlayer2.spotify", "Spotify"));
        assert!(!prefers("Spotify", "org.mpris.MediaPlayer2.spotify", "mpv"));
    }

    /// A player that has not filled in its identity must not match everything
    /// by virtue of the empty string being a substring of every name.
    #[test]
    fn a_nameless_player_matches_nothing_by_accident() {
        assert!(!prefers("", "org.mpris.MediaPlayer2.vlc", "spotify"));
    }

    #[test]
    fn a_watcher_starts_and_stops_without_a_player() {
        let m = Mpris::start();
        m.prefer(Some("nothing-by-this-name".into()));
        std::thread::sleep(Duration::from_millis(50));
        // Dropping must not hang waiting on the poll thread.
        drop(m);
    }
}
