//! Cutting a tape out of the log.
//!
//! The listening log is the archive: every play, in order, across every
//! service, never rewritten. A playlist is a *view* of it — pick a month,
//! collapse the repeats, and you have what you actually listened to in a
//! format any player can open. Nothing about that requires the services to
//! cooperate, and none of them offer it.
//!
//! This is deliberately a separate verb from recording. The deck records; what
//! you cut out of the log afterwards is a different job, done from the command
//! line rather than the panel, because it takes arguments a panel has no
//! sensible way to ask for.
//!
//! ## What projection decides, and what it does not
//!
//! Deduplication lives here rather than in the log, because how often you
//! played something is the signal and the log is not allowed to lose it. The
//! playlist that comes out collapses repeats and can order by them.
//!
//! What it will not do is invent a location. A browser tab routinely reports a
//! title and nothing else, and a playlist entry with no path is not a track —
//! so those come out as comments, counted and named, rather than as lines that
//! would look playable and fail to cue.

use std::collections::HashMap;

use crate::listen::Entry;

/// Which part of the log to cut from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Everything ever logged.
    All,
    /// The most recent session — "what I just listened to".
    Last,
    /// A session id or any leading part of one, with punctuation ignored, so
    /// `2026-07`, `202607` and `2026-07-26` all select what you would expect.
    Prefix(String),
}

impl Pick {
    /// Parse a selector, or say why it is not one.
    ///
    /// A selector with no digits in it cannot be rejected quietly: reducing
    /// `--export=july` to an empty prefix would match every session, so a typo
    /// would hand back the entire history under a header that declines to say
    /// what it is. Widening silently is the worst of the available failures.
    pub fn parse(s: &str) -> Result<Pick, String> {
        Ok(match s.trim() {
            "" | "all" => Pick::All,
            "last" => Pick::Last,
            other => {
                let d = digits(other);
                if d.is_empty() {
                    return Err(format!(
                        "'{other}' selects nothing — use a date (2026-07), a session id, \
                         'last', or 'all'. 'ten-qd --log' lists what is there"
                    ));
                }
                Pick::Prefix(d)
            }
        })
    }

    fn describe(&self) -> String {
        match self {
            Pick::All => "everything".into(),
            Pick::Last => "the last session".into(),
            // Printed back in the shape it selects rather than as the run of
            // digits it was reduced to, so the playlist's own header says
            // "2026-07" whichever way the operator typed it.
            Pick::Prefix(p) => match p.len() {
                0..=4 => p.clone(),
                5..=6 => format!("{}-{}", &p[..4], &p[4..]),
                7..=8 => format!("{}-{}-{}", &p[..4], &p[4..6], &p[6..]),
                _ => format!("{}-{}", &p[..8], &p[8..]),
            },
        }
    }
}

/// Just the digits, so a session id and a date the operator typed can be
/// compared without caring which separators either of them used.
fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

// ---------------------------------------------------------------------------
// Reading it back
// ---------------------------------------------------------------------------

/// What a read of the log found, including what it could not use.
pub struct Read {
    pub entries: Vec<Entry>,
    /// Lines that would not parse. Reported rather than swallowed: append-only
    /// means a crash can leave one torn line at the end, which is discardable
    /// on its own — but *many* unreadable lines mean something else is wrong
    /// and the operator should hear about it.
    pub torn: usize,
}

pub fn read(text: &str) -> Read {
    let mut entries = Vec::new();
    let mut torn = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Entry>(line) {
            Ok(e) => entries.push(e),
            Err(_) => torn += 1,
        }
    }
    entries.sort_by(|a, b| a.at.cmp(&b.at));
    Read { entries, torn }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// One run of the rack, summarised — enough to choose one by.
pub struct Session {
    pub id: String,
    /// When the first entry in it started.
    pub started: String,
    pub entries: usize,
    /// Total time played, which is not the length of the session: the rack can
    /// sit idle for an hour between tracks.
    pub seconds: u64,
    pub players: Vec<String>,
}

/// Group the log by session, most recent last, which is the order it was
/// written in and the order a listing reads best in.
pub fn sessions(entries: &[Entry]) -> Vec<Session> {
    let mut out: Vec<Session> = Vec::new();
    for e in entries {
        match out.iter_mut().find(|s| s.id == e.session) {
            Some(s) => {
                s.entries += 1;
                s.seconds += e.seconds;
                if !s.players.contains(&e.player) {
                    s.players.push(e.player.clone());
                }
            }
            None => out.push(Session {
                id: e.session.clone(),
                started: e.at.clone(),
                entries: 1,
                seconds: e.seconds,
                players: vec![e.player.clone()],
            }),
        }
    }
    out.sort_by(|a, b| a.started.cmp(&b.started));
    out
}

/// Narrow the log to what was asked for.
pub fn select(entries: &[Entry], pick: &Pick) -> Vec<Entry> {
    match pick {
        Pick::All => entries.to_vec(),
        Pick::Last => {
            let last = sessions(entries).pop().map(|s| s.id).unwrap_or_default();
            entries.iter().filter(|e| e.session == last).cloned().collect()
        }
        Pick::Prefix(p) => {
            entries.iter().filter(|e| digits(&e.session).starts_with(p)).cloned().collect()
        }
    }
}

// ---------------------------------------------------------------------------
// The cut
// ---------------------------------------------------------------------------

/// One track of the projection, after the repeats have been collapsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    pub artist: String,
    pub title: String,
    /// Empty when nothing ever said where the track lives.
    pub uri: String,
    /// The longest single play seen.
    ///
    /// The log records how long *you played* something, which for a track you
    /// heard five times is five different numbers. The longest is the closest
    /// anyone can get to the track's own length from this side, and length is
    /// what a playlist's `#EXTINF` is for — it is how sides get split.
    pub seconds: u64,
    pub plays: u32,
    /// The first time it was heard, which is the order a mixtape is in.
    pub first: String,
}

/// Collapse plays into tracks.
///
/// `rank` orders by how often something was played rather than by when it was
/// first heard. Both are real artefacts and they are not the same one: play
/// count is your taste, and first-heard is the *order*, which is the thing a
/// mixtape actually was.
pub fn cut(entries: &[Entry], rank: bool) -> Vec<Cut> {
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    let mut cuts: Vec<Cut> = Vec::new();

    for e in entries {
        let key = (e.artist.to_lowercase(), e.title.to_lowercase());
        match index.get(&key) {
            Some(&i) => {
                let c = &mut cuts[i];
                c.plays += 1;
                c.seconds = c.seconds.max(e.seconds);
                // A location learned late still counts. Spotify reports one and
                // a browser often does not, and the same song can arrive both
                // ways in one week.
                if c.uri.is_empty() {
                    c.uri = e.uri.clone();
                }
            }
            None => {
                index.insert(key, cuts.len());
                cuts.push(Cut {
                    artist: e.artist.clone(),
                    title: e.title.clone(),
                    uri: e.uri.clone(),
                    seconds: e.seconds,
                    plays: 1,
                    first: e.at.clone(),
                });
            }
        }
    }

    if rank {
        // Ties keep their first-heard order, so a ranked list is still a
        // playable sequence rather than an arbitrary one.
        cuts.sort_by(|a, b| b.plays.cmp(&a.plays).then_with(|| a.first.cmp(&b.first)));
    }
    cuts
}

/// Flatten anything that would break the one-record-per-line format.
///
/// A browser tab's title is arbitrary text and reaches the log intact, because
/// JSON escapes a newline rather than refusing it. Interpolated raw into an
/// `#EXTINF` it would split the line, and the tail would read back as a path —
/// a track named after half a title, pointing at nothing.
fn oneline(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().into()
}

/// Render an M3U.
///
/// Only tracks with a location get an `#EXTINF` and a path. The rest are
/// comments — `playlist.rs` reads an `#EXTINF` as a label for the *next*
/// non-comment line, so emitting one for a track with nowhere to point would
/// hand its title to whatever came after it.
pub fn m3u(cuts: &[Cut], what: &Pick) -> String {
    let playable = cuts.iter().filter(|c| !c.uri.is_empty()).count();
    let mut out = String::from("#EXTM3U\n");
    out.push_str(&format!(
        "#PLAYLIST:ten-qd · {} · {playable} tracks\n",
        what.describe()
    ));

    for c in cuts {
        let label = if c.artist.is_empty() {
            oneline(&c.title)
        } else {
            format!("{} - {}", oneline(&c.artist), oneline(&c.title))
        };
        if c.uri.is_empty() {
            out.push_str(&format!("# no location · {} play(s) · {label}\n", c.plays));
            continue;
        }
        if c.plays > 1 {
            out.push_str(&format!("#PLAYS:{}\n", c.plays));
        }
        // The location gets the same treatment as the label, for the same
        // reason: `uri` is whatever the player put in `xesam:url`, and a
        // control character in it would split this line too.
        out.push_str(&format!("#EXTINF:{},{label}\n{}\n", c.seconds, oneline(&c.uri)));
    }
    out
}

/// One line per session, for a listing.
pub fn describe(s: &Session) -> String {
    let (h, m) = (s.seconds / 3600, (s.seconds % 3600) / 60);
    format!(
        "{:<16}  {}  {:>4} entries  {h:>2}h {m:02}m  {}",
        s.id,
        s.started.replace('T', " ").trim_end_matches('Z'),
        s.entries,
        s.players.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(session: &str, at: &str, artist: &str, title: &str, uri: &str, secs: u64) -> Entry {
        Entry {
            at: at.into(),
            session: session.into(),
            player: "Spotify".into(),
            artist: artist.into(),
            title: title.into(),
            album: String::new(),
            seconds: secs,
            uri: uri.into(),
        }
    }

    fn log() -> Vec<Entry> {
        vec![
            e("20260726-100000", "2026-07-26T10:00:00Z", "BoC", "Cold Earth", "spotify:a", 100),
            e("20260726-100000", "2026-07-26T10:02:00Z", "BoC", "Reach", "spotify:b", 200),
            e("20260726-100000", "2026-07-26T10:06:00Z", "BoC", "cold earth", "spotify:a", 214),
            e("20260805-200000", "2026-08-05T20:00:00Z", "", "Playback | Omniguard", "", 300),
        ]
    }

    #[test]
    fn a_selector_ignores_whatever_punctuation_was_typed() {
        let p = |s: &str| Pick::parse(s).expect("a selector");
        assert_eq!(p("2026-07"), Pick::Prefix("202607".into()));
        assert_eq!(p("202607"), Pick::Prefix("202607".into()));
        assert_eq!(p("20260726-100000"), Pick::Prefix("20260726100000".into()));
        assert_eq!(p(""), Pick::All);
        assert_eq!(p(" last "), Pick::Last);
    }

    /// A selector reducing to no digits used to match every session, so
    /// `--export=july` quietly handed back the entire history.
    #[test]
    fn a_selector_with_no_digits_is_refused_rather_than_widened() {
        for bad in ["july", "foo", "--rank", "last week"] {
            let why = Pick::parse(bad).expect_err(bad);
            assert!(why.contains(bad), "the message must name what was typed: {why}");
            assert!(why.contains("--log"), "and where to look instead: {why}");
        }
    }

    #[test]
    fn a_date_prefix_selects_the_sessions_inside_it() {
        let all = log();
        assert_eq!(select(&all, &Pick::Prefix("202607".into())).len(), 3);
        assert_eq!(select(&all, &Pick::Prefix("202608".into())).len(), 1);
        assert_eq!(select(&all, &Pick::Prefix("2026".into())).len(), 4);
        assert!(select(&all, &Pick::Prefix("1999".into())).is_empty());
    }

    #[test]
    fn last_means_the_most_recent_run_of_the_rack() {
        let picked = select(&log(), &Pick::Last);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].session, "20260805-200000");
        assert!(select(&[], &Pick::Last).is_empty(), "an empty log must not panic");
    }

    #[test]
    fn sessions_are_summarised_by_what_they_contain() {
        let s = sessions(&log());
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].id, "20260726-100000");
        assert_eq!(s[0].entries, 3);
        assert_eq!(s[0].seconds, 514);
        assert_eq!(s[0].players, vec!["Spotify"]);
    }

    #[test]
    fn repeats_collapse_but_the_count_survives() {
        let cuts = cut(&log(), false);
        assert_eq!(cuts.len(), 3, "cold earth and Cold Earth are one track");
        assert_eq!(cuts[0].plays, 2);
        assert_eq!(cuts[0].seconds, 214, "the longest play is the best length there is");
        assert_eq!(cuts[0].first, "2026-07-26T10:00:00Z", "kept in the order first heard");
    }

    #[test]
    fn ranking_orders_by_plays_and_keeps_the_order_within_a_tie() {
        let cuts = cut(&log(), true);
        assert_eq!(cuts[0].title, "Cold Earth");
        assert_eq!(cuts[1].title, "Reach", "a tie stays in the order it was heard");
        assert_eq!(cuts[2].title, "Playback | Omniguard");
    }

    /// A location that only one of the services reports still counts. The same
    /// song from a browser and from Spotify is one track, and the playable
    /// half of it is what makes the entry cue.
    #[test]
    fn a_location_learned_late_is_kept() {
        let entries = vec![
            e("s", "2026-07-26T10:00:00Z", "BoC", "Cold Earth", "", 100),
            e("s", "2026-07-26T10:02:00Z", "BoC", "Cold Earth", "spotify:a", 100),
        ];
        assert_eq!(cut(&entries, false)[0].uri, "spotify:a");
    }

    #[test]
    fn a_track_with_nowhere_to_point_is_a_comment_not_an_entry() {
        let text = m3u(&cut(&log(), false), &Pick::All);
        assert!(text.contains("# no location · 1 play(s) · Playback | Omniguard"), "{text}");
        assert!(
            !text.contains("#EXTINF:300"),
            "an EXTINF with no path would label the next track: {text}"
        );
        assert!(text.contains("#PLAYLIST:ten-qd · everything · 2 tracks"), "{text}");
    }

    /// A browser tab's title is arbitrary text and reaches the log intact,
    /// because JSON escapes a newline rather than refusing it. Interpolated
    /// raw it would split the `#EXTINF` and the tail would read back as a path.
    #[test]
    fn a_title_cannot_break_the_line_it_is_written_on() {
        let entries = vec![e(
            "s",
            "2026-07-26T10:00:00Z",
            "BoC\nspotify:track:evil",
            "Cold\tEarth",
            "spotify:a",
            100,
        )];
        let text = m3u(&cut(&entries, false), &Pick::All);
        let extinf: Vec<&str> = text.lines().filter(|l| l.starts_with("#EXTINF:")).collect();
        assert_eq!(extinf.len(), 1, "{text}");
        assert_eq!(extinf[0], "#EXTINF:100,BoC spotify:track:evil - Cold Earth");
        assert_eq!(text.lines().count(), 4, "header, playlist, extinf, location: {text}");
    }

    #[test]
    fn a_projection_reads_back_as_the_playlist_it_claims_to_be() {
        let text = m3u(&cut(&log(), false), &Pick::All);
        assert!(text.starts_with("#EXTM3U\n"));
        assert!(text.contains("#PLAYS:2\n#EXTINF:214,BoC - Cold Earth\nspotify:a\n"), "{text}");
        assert!(text.contains("#EXTINF:200,BoC - Reach\nspotify:b\n"), "{text}");
        // Nothing lost and nothing invented: every cut appears exactly once.
        assert_eq!(text.matches("BoC - Cold Earth").count(), 1);
    }

    #[test]
    fn a_torn_line_is_counted_rather_than_abandoning_the_history() {
        let good = r#"{"at":"2026-07-26T10:00:00Z","session":"s","player":"Spotify","artist":"a","title":"t","seconds":9}"#;
        let r = read(&format!("{good}\n{{\"at\":\"2026-07-2\n\n{good}\n"));
        assert_eq!(r.entries.len(), 2, "a crash at the end must not cost the file");
        assert_eq!(r.torn, 1);
    }

    #[test]
    fn entries_come_back_in_the_order_they_were_heard() {
        let a = r#"{"at":"2026-07-26T11:00:00Z","session":"s","title":"second"}"#;
        let b = r#"{"at":"2026-07-26T10:00:00Z","session":"s","title":"first"}"#;
        let r = read(&format!("{a}\n{b}\n"));
        assert_eq!(r.entries[0].title, "first");
    }

    #[test]
    fn an_empty_log_projects_to_an_empty_playlist_rather_than_failing() {
        let r = read("");
        assert!(r.entries.is_empty());
        assert!(sessions(&r.entries).is_empty());
        let text = m3u(&cut(&r.entries, true), &Pick::All);
        assert_eq!(text.lines().count(), 2, "a header and nothing else: {text}");
    }
}
