//! Playlist files.
//!
//! A tape can be compiled from a folder, or handed to you as a list. Both of
//! these formats predate the streaming era by a wide margin and are still what
//! everything exports, which is the only reason to support them: nothing here
//! is invented.
//!
//! - **M3U / M3U8** — one path per line, `#EXTINF:seconds,Artist - Title`
//!   carrying an optional label. M3U8 is the same thing, guaranteed UTF-8.
//! - **PLS** — the Winamp INI shape: `File1=`, `Title1=`, `Length1=`.
//!
//! Relative paths resolve against the playlist file's own directory, which is
//! what makes a playlist portable alongside the music it points at.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// What a playlist turned out to hold.
pub struct Loaded {
    pub entries: Vec<Entry>,
    /// Lines naming something the deck cannot cue — a station, or a service
    /// URI out of the listening log. Reported rather than silently dropped: a
    /// tape cut from a week of Spotify and mpv loads with half its tracks
    /// missing, and "12 tracks" with no further comment is how that becomes
    /// invisible.
    pub remote: usize,
}

/// One line of a playlist, before the file it names has been opened.
pub struct Entry {
    pub path: PathBuf,
    /// The label the playlist carried, if any. Metadata read from the file
    /// itself wins over this — a playlist's idea of a title is hearsay.
    pub title: Option<String>,
}

pub fn is_playlist(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("m3u" | "m3u8" | "pls")
    )
}

/// Read a playlist into entries. Anything with a URI scheme other than `file`
/// is skipped: this is a cassette deck, and a line pointing at `http://` is a
/// station while `spotify:track:…` is a song the deck has no way to reach.
pub fn read(path: &Path) -> Result<Loaded> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    let (entries, remote) = match ext.as_str() {
        "pls" => parse_pls(&text, dir),
        _ => parse_m3u(&text, dir),
    };

    if entries.is_empty() {
        // A tape cut out of the listening log is mostly service URIs, and
        // "lists nothing playable" would be a true but useless thing to say
        // about it. Whether such a tape can replay depends on the service; the
        // deck can at least name the reason it cannot cue.
        if remote > 0 {
            bail!(
                "{} lists {remote} stream(s) or service URI(s) and no files — \
                 the deck cues files",
                path.display()
            );
        }
        bail!("{} lists nothing playable", path.display());
    }
    Ok(Loaded { entries, remote })
}

/// The URI scheme a line starts with, if it starts with one at all.
///
/// Needed because not every URI has a `//` after the colon: a tape cut out of
/// the listening log carries `spotify:track:…`, and treating that as a
/// relative path would have the deck looking for a file of that name in the
/// playlist's own folder.
///
/// Syntax alone cannot finish the job — `Autechre:Gantz Graf.flac` is a
/// perfectly good scheme by these rules and a perfectly good filename by any
/// ripper's — so this only reports the shape, and `resolve` decides.
fn scheme(raw: &str) -> Option<&str> {
    let (head, _) = raw.split_once(':')?;
    let ok = !head.is_empty()
        && head.starts_with(|c: char| c.is_ascii_alphabetic())
        && head.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    ok.then_some(head)
}

fn resolve(raw: &str, dir: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // A stream URL or a service URI is not a track. `file://` is, once
    // unwrapped.
    if let Some(s) = scheme(raw) {
        let rest = &raw[s.len() + 1..];
        if s.eq_ignore_ascii_case("file") {
            // `file://…` is absolute; the rarer `file:a.flac` is relative and
            // resolves against the playlist like any other relative line.
            let p = PathBuf::from(percent_decode(rest.strip_prefix("//").unwrap_or(rest)));
            return Some(if p.is_absolute() { p } else { dir.join(p) });
        }
        // `scheme://…` is a URI beyond argument. Without the slashes it is
        // genuinely ambiguous: `spotify:track:…` is a service URI, and
        // `Autechre:Gantz Graf.flac` is a file, named by a ripper that had no
        // opinion about URI syntax. The extension settles it, because the deck
        // cues audio files and no service URI ends in one.
        if rest.starts_with("//") || !crate::disc::is_audio(Path::new(raw)) {
            return None;
        }
    }
    let p = PathBuf::from(raw);
    Some(if p.is_absolute() { p } else { dir.join(p) })
}

/// Minimal percent-decoding, enough for the `file://` URLs playlists carry.
/// Not a general URL decoder and does not need to be.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Returns the entries, and how many lines named something that is not a file
/// — a station, or a service URI out of the listening log.
fn parse_m3u(text: &str, dir: &Path) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let mut remote = 0;
    let mut pending: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // `#EXTINF:seconds,Artist - Title` — the label is after the comma,
            // and the duration is ignored because the file itself knows better.
            pending = rest.split_once(',').map(|(_, t)| t.trim().to_string()).filter(|t| !t.is_empty());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(path) = resolve(line, dir) {
            out.push(Entry { path, title: pending.take() });
        } else {
            remote += 1;
            pending = None;
        }
    }
    (out, remote)
}

fn parse_pls(text: &str, dir: &Path) -> (Vec<Entry>, usize) {
    // PLS is indexed rather than ordered, so collect by index and sort.
    let mut files: Vec<(u32, PathBuf)> = Vec::new();
    let mut titles: Vec<(u32, String)> = Vec::new();
    let mut remote = 0;

    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        let value = value.trim();

        let split = |prefix: &str| -> Option<u32> {
            key.strip_prefix(prefix)
                .filter(|_| key.len() > prefix.len())
                .and_then(|n| n.parse().ok())
        };

        if key.eq_ignore_ascii_case("NumberOfEntries") || key.eq_ignore_ascii_case("Version") {
            continue;
        }
        if let Some(n) = split("File").or_else(|| split("file")) {
            match resolve(value, dir) {
                Some(p) => files.push((n, p)),
                None => remote += 1,
            }
        } else if let Some(n) = split("Title").or_else(|| split("title"))
            && !value.is_empty()
        {
            titles.push((n, value.to_string()));
        }
    }

    files.sort_by_key(|(n, _)| *n);
    let out = files
        .into_iter()
        .map(|(n, path)| Entry {
            title: titles.iter().find(|(m, _)| *m == n).map(|(_, t)| t.clone()),
            path,
        })
        .collect();
    (out, remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_relative_paths_resolve_against_the_playlist() {
        let e = parse_m3u("a.flac\nsub/b.flac\n", Path::new("/music/mix")).0;
        assert_eq!(e[0].path, PathBuf::from("/music/mix/a.flac"));
        assert_eq!(e[1].path, PathBuf::from("/music/mix/sub/b.flac"));
    }

    #[test]
    fn m3u_absolute_paths_are_left_alone() {
        let e = parse_m3u("/elsewhere/c.flac\n", Path::new("/music/mix")).0;
        assert_eq!(e[0].path, PathBuf::from("/elsewhere/c.flac"));
    }

    #[test]
    fn extinf_labels_attach_to_the_line_below_them() {
        let e = parse_m3u(
            "#EXTM3U\n#EXTINF:213,Ficsit Inc.\na.flac\nb.flac\n",
            Path::new("/m"),
        ).0;
        assert_eq!(e[0].title.as_deref(), Some("Ficsit Inc."));
        assert_eq!(e[1].title, None, "a label must not leak onto the next track");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let e = parse_m3u("#EXTM3U\n\n# a note\na.flac\n\n", Path::new("/m")).0;
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn stream_urls_are_not_tracks() {
        let e = parse_m3u("http://stream.example/live\na.flac\n", Path::new("/m")).0;
        assert_eq!(e.len(), 1, "a station is not a track: {:?}", e[0].path);
        assert_eq!(e[0].path, PathBuf::from("/m/a.flac"));
    }

    /// A tape cut out of the listening log carries `spotify:track:…`, which
    /// has no `//` in it. Read as a relative path it would send the deck
    /// looking for a file of that name next to the playlist.
    #[test]
    fn a_service_uri_is_not_a_relative_path() {
        let (e, remote) = parse_m3u("spotify:track:aaa\nmpris:x\na.flac\n", Path::new("/m"));
        assert_eq!(e.len(), 1, "got {:?}", e.iter().map(|x| &x.path).collect::<Vec<_>>());
        assert_eq!(e[0].path, PathBuf::from("/m/a.flac"));
        assert_eq!(remote, 2);
    }

    /// …while a filename that merely contains a colon still resolves.
    ///
    /// `Artist:Title.ext` is a common ripper convention, and by URI syntax
    /// alone `Autechre:` is a perfectly good scheme. The extension is what
    /// settles it, because the deck cues audio files and no service URI ends
    /// in one.
    #[test]
    fn a_colon_in_a_filename_is_not_a_scheme() {
        for name in [
            "Boards of Canada: Cold Earth.flac",
            "Autechre:Gantz Graf.flac",
            "Autechre:GantzGraf.flac",
            "Amber-Autechre:01.mp3",
        ] {
            let e = parse_m3u(&format!("{name}\n"), Path::new("/m")).0;
            assert_eq!(e.len(), 1, "{name} was turned away as a URI");
            assert_eq!(e[0].path, PathBuf::from(format!("/m/{name}")));
        }
    }

    /// The other half: a stream whose URL happens to end in an audio
    /// extension is still a stream, because `//` settles it first.
    #[test]
    fn a_stream_that_looks_like_a_file_is_still_a_stream() {
        let (e, remote) = parse_m3u("http://stream.example/live.mp3\n", Path::new("/m"));
        assert!(e.is_empty(), "got {:?}", e.iter().map(|x| &x.path).collect::<Vec<_>>());
        assert_eq!(remote, 1);
    }

    /// The message an operator gets when they hand the deck a tape cut out of
    /// the log. "Lists nothing playable" would be true and useless.
    #[test]
    fn a_tape_of_service_uris_says_why_it_will_not_cue() {
        let dir = std::env::temp_dir().join(format!("ten-qd-pl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("mix.m3u");
        std::fs::write(&file, "#EXTM3U\nspotify:track:aaa\nspotify:track:bbb\n").expect("write");

        let why = match read(&file) {
            Err(e) => e.to_string(),
            Ok(l) => panic!("claimed {} playable track(s) out of service URIs", l.entries.len()),
        };
        std::fs::remove_dir_all(&dir).ok();
        assert!(why.contains("2 stream(s) or service URI(s)"), "{why}");
        assert!(why.contains("the deck cues files"), "{why}");
    }

    #[test]
    fn pls_counts_what_it_could_not_use() {
        let (e, remote) = parse_pls(
            "[playlist]\nFile1=http://stream/live\nFile2=a.flac\n",
            Path::new("/m"),
        );
        assert_eq!(e.len(), 1);
        assert_eq!(remote, 1);
    }

    #[test]
    fn file_urls_are_unwrapped_and_decoded() {
        let e = parse_m3u("file:///music/Ficsit%20Inc..flac\n", Path::new("/m")).0;
        assert_eq!(e[0].path, PathBuf::from("/music/Ficsit Inc..flac"));
    }

    /// The rarer slashless form is relative, and resolves against the playlist
    /// like any other relative line rather than against the process's cwd.
    #[test]
    fn a_file_url_without_slashes_is_relative_to_the_playlist() {
        let e = parse_m3u("file:a.flac\n", Path::new("/m")).0;
        assert_eq!(e[0].path, PathBuf::from("/m/a.flac"));
    }

    #[test]
    fn pls_orders_by_index_not_by_line() {
        let e = parse_pls(
            "[playlist]\nFile2=b.flac\nTitle2=Bee\nFile1=a.flac\nTitle1=Ay\nNumberOfEntries=2\n",
            Path::new("/m"),
        ).0;
        assert_eq!(e[0].path, PathBuf::from("/m/a.flac"));
        assert_eq!(e[0].title.as_deref(), Some("Ay"));
        assert_eq!(e[1].path, PathBuf::from("/m/b.flac"));
        assert_eq!(e[1].title.as_deref(), Some("Bee"));
    }

    #[test]
    fn pls_ignores_its_own_header_keys() {
        let e = parse_pls("[playlist]\nNumberOfEntries=1\nVersion=2\nFile1=a.flac\n", Path::new("/m")).0;
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn extension_detection_is_case_insensitive() {
        assert!(is_playlist(Path::new("x.M3U")));
        assert!(is_playlist(Path::new("x.m3u8")));
        assert!(is_playlist(Path::new("x.PLS")));
        assert!(!is_playlist(Path::new("x.flac")));
    }
}
