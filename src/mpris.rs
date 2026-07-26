//! Reaching over to hit next on the Discman.
//!
//! A real cassette adapter was a one-way cable. The deck's transport keys did
//! nothing to the thing on the other end — you reached over to the passenger
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
    /// The player's own name for itself: "Spotify", "Chromium", "mpv".
    pub player: String,
    pub title: String,
    pub artist: String,
    pub album: String,
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
    /// The application name to prefer when several players are running —
    /// normally whatever is plugged into the adapter.
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
        let prefer: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));

        let (t_now, t_prefer, t_stop) = (now.clone(), prefer.clone(), stop.clone());
        std::thread::Builder::new()
            .name("ten-qd/mpris".into())
            .spawn(move || {
                while !t_stop.load(Ordering::Relaxed) {
                    let want = t_prefer.lock().ok().and_then(|g| g.clone());
                    let found = poll(want.as_deref());
                    if let Ok(mut g) = t_now.lock() {
                        *g = found;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .ok();

        Mpris { now, prefer, stop }
    }

    /// Tell the watcher which player to prefer. Several can be running at
    /// once — a browser and Spotify — and the right one is whichever is
    /// plugged into the adapter.
    pub fn prefer(&self, app: Option<String>) {
        if let Ok(mut g) = self.prefer.lock() {
            *g = app;
        }
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        self.now.lock().ok().and_then(|g| g.clone())
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

/// Pick a player, preferring one whose identity or bus name resembles `want`.
fn find(want: Option<&str>) -> Option<mpris::Player> {
    let finder = mpris::PlayerFinder::new().ok()?;
    let players = finder.find_all().ok()?;
    if players.is_empty() {
        return None;
    }

    if let Some(w) = want {
        let w = w.to_ascii_lowercase();
        // Match loosely: PipeWire calls it "Spotify", MPRIS calls it
        // "Spotify", but a browser is "Chromium" one side and
        // "chromium.instance3976392" the other.
        if let Some(p) = players.iter().position(|p| {
            let id = p.identity().to_ascii_lowercase();
            let bus = p.bus_name().to_ascii_lowercase();
            id.contains(&w) || w.contains(&id) || bus.contains(&w)
        }) {
            return players.into_iter().nth(p);
        }
    }

    // No preference, or nothing matched: whichever is actually playing wins
    // over one merely sitting paused.
    let playing = players
        .iter()
        .position(|p| matches!(p.get_playback_status(), Ok(mpris::PlaybackStatus::Playing)));
    match playing {
        Some(i) => players.into_iter().nth(i),
        None => players.into_iter().next(),
    }
}

fn poll(want: Option<&str>) -> Option<NowPlaying> {
    let player = find(want)?;
    let meta = player.get_metadata().ok();
    Some(NowPlaying {
        player: player.identity().to_string(),
        playing: matches!(player.get_playback_status(), Ok(mpris::PlaybackStatus::Playing)),
        title: meta.as_ref().and_then(|m| m.title()).unwrap_or_default().to_string(),
        artist: meta
            .as_ref()
            .and_then(|m| m.artists())
            .map(|a| a.join(", "))
            .unwrap_or_default(),
        album: meta.as_ref().and_then(|m| m.album_name()).unwrap_or_default().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the real bus when one is present. Read-only, so it is safe
    /// anywhere; a machine with no D-Bus or no player reports nothing rather
    /// than failing.
    #[test]
    fn polling_either_finds_a_player_or_reports_none() {
        if let Some(n) = poll(None) {
            assert!(!n.player.is_empty(), "a found player must name itself");
        }
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
