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

/// Read a playlist into entries. Remote URLs are skipped: this is a cassette
/// deck, and a line pointing at `http://` is a station, not a track.
pub fn read(path: &Path) -> Result<Vec<Entry>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    let entries = match ext.as_str() {
        "pls" => parse_pls(&text, dir),
        _ => parse_m3u(&text, dir),
    };

    if entries.is_empty() {
        bail!("{} lists nothing playable", path.display());
    }
    Ok(entries)
}

fn resolve(raw: &str, dir: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // A stream URL is not a track. `file://` is, once unwrapped.
    if let Some(rest) = raw.strip_prefix("file://") {
        return Some(PathBuf::from(percent_decode(rest)));
    }
    if raw.contains("://") {
        return None;
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

fn parse_m3u(text: &str, dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
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
            pending = None;
        }
    }
    out
}

fn parse_pls(text: &str, dir: &Path) -> Vec<Entry> {
    // PLS is indexed rather than ordered, so collect by index and sort.
    let mut files: Vec<(u32, PathBuf)> = Vec::new();
    let mut titles: Vec<(u32, String)> = Vec::new();

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
            if let Some(p) = resolve(value, dir) {
                files.push((n, p));
            }
        } else if let Some(n) = split("Title").or_else(|| split("title"))
            && !value.is_empty()
        {
            titles.push((n, value.to_string()));
        }
    }

    files.sort_by_key(|(n, _)| *n);
    files
        .into_iter()
        .map(|(n, path)| Entry {
            title: titles.iter().find(|(m, _)| *m == n).map(|(_, t)| t.clone()),
            path,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_relative_paths_resolve_against_the_playlist() {
        let e = parse_m3u("a.flac\nsub/b.flac\n", Path::new("/music/mix"));
        assert_eq!(e[0].path, PathBuf::from("/music/mix/a.flac"));
        assert_eq!(e[1].path, PathBuf::from("/music/mix/sub/b.flac"));
    }

    #[test]
    fn m3u_absolute_paths_are_left_alone() {
        let e = parse_m3u("/elsewhere/c.flac\n", Path::new("/music/mix"));
        assert_eq!(e[0].path, PathBuf::from("/elsewhere/c.flac"));
    }

    #[test]
    fn extinf_labels_attach_to_the_line_below_them() {
        let e = parse_m3u(
            "#EXTM3U\n#EXTINF:213,Ficsit Inc.\na.flac\nb.flac\n",
            Path::new("/m"),
        );
        assert_eq!(e[0].title.as_deref(), Some("Ficsit Inc."));
        assert_eq!(e[1].title, None, "a label must not leak onto the next track");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let e = parse_m3u("#EXTM3U\n\n# a note\na.flac\n\n", Path::new("/m"));
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn stream_urls_are_not_tracks() {
        let e = parse_m3u("http://stream.example/live\na.flac\n", Path::new("/m"));
        assert_eq!(e.len(), 1, "a station is not a track: {:?}", e[0].path);
        assert_eq!(e[0].path, PathBuf::from("/m/a.flac"));
    }

    #[test]
    fn file_urls_are_unwrapped_and_decoded() {
        let e = parse_m3u("file:///music/Ficsit%20Inc..flac\n", Path::new("/m"));
        assert_eq!(e[0].path, PathBuf::from("/music/Ficsit Inc..flac"));
    }

    #[test]
    fn pls_orders_by_index_not_by_line() {
        let e = parse_pls(
            "[playlist]\nFile2=b.flac\nTitle2=Bee\nFile1=a.flac\nTitle1=Ay\nNumberOfEntries=2\n",
            Path::new("/m"),
        );
        assert_eq!(e[0].path, PathBuf::from("/m/a.flac"));
        assert_eq!(e[0].title.as_deref(), Some("Ay"));
        assert_eq!(e[1].path, PathBuf::from("/m/b.flac"));
        assert_eq!(e[1].title.as_deref(), Some("Bee"));
    }

    #[test]
    fn pls_ignores_its_own_header_keys() {
        let e = parse_pls("[playlist]\nNumberOfEntries=1\nVersion=2\nFile1=a.flac\n", Path::new("/m"));
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
