//! Reading a disc.
//!
//! A directory of audio files *is* a disc. Its sorted file order is the table
//! of contents, and that TOC is read once at load time — the same way a real
//! player spins up, reads the lead-in, and then knows the track count and total
//! time before it plays a note. That is why `load` opens every file: the
//! display cannot honestly show a track count it has not verified.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};

use crate::state::{Disc, Track};

const EXTENSIONS: &[&str] = &["flac", "mp3", "ogg", "oga", "m4a", "mp4", "aac", "wav", "opus"];

pub fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// What one file reports about itself.
struct Probed {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    track_no: Option<u64>,
    seconds: f64,
}

fn probe_file(path: &Path) -> Result<Probed> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut reader = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .with_context(|| format!("unreadable: {}", path.display()))?;

    let track = reader
        .default_track(TrackType::Audio)
        .context("no audio track")?;

    // Prefer the container's stated duration in timebase units; fall back to
    // the playable frame count over the sample rate. Either way the figure
    // comes from the file, not from a guess.
    let seconds = track
        .duration
        .zip(track.time_base)
        .map(|(d, tb)| d.get() as f64 * f64::from(tb))
        .or_else(|| {
            let rate = match track.codec_params.as_ref() {
                Some(symphonia::core::codecs::CodecParameters::Audio(a)) => a.sample_rate,
                _ => None,
            }?;
            Some(track.num_frames? as f64 / rate as f64)
        })
        .unwrap_or(0.0);

    let mut out = Probed { title: None, artist: None, album: None, track_no: None, seconds };

    // Metadata may live in the container (Vorbis comments) or in a wrapping
    // ID3 chunk the probe peeled off first. Check both.
    let mut harvest = |rev: &symphonia::core::meta::MetadataRevision| {
        for tag in &rev.media.tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(v)) => out.title.get_or_insert_with(|| v.to_string()),
                Some(StandardTag::Artist(v)) => out.artist.get_or_insert_with(|| v.to_string()),
                Some(StandardTag::AlbumArtist(v)) => {
                    out.artist.get_or_insert_with(|| v.to_string())
                }
                Some(StandardTag::Album(v)) => out.album.get_or_insert_with(|| v.to_string()),
                Some(StandardTag::TrackNumber(n)) => {
                    out.track_no.get_or_insert(*n);
                    continue;
                }
                #[allow(unreachable_patterns)]
                _ => continue,
            };
        }
    };

    if let Some(rev) = reader.metadata().current() {
        harvest(rev);
    }

    Ok(out)
}

/// Read one file into a `Track`. Used by the browser when compiling a tape
/// from files that do not share a directory.
pub fn probe_track(path: &Path) -> Result<Track> {
    let p = probe_file(path)?;
    Ok(Track {
        title: p
            .title
            .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into_owned()),
        artist: p.artist.unwrap_or_else(|| "—".into()),
        seconds: p.seconds,
        path: path.to_path_buf(),
    })
}

/// Load a directory as a disc. Files that will not open are skipped rather
/// than aborting the load — one bad file should not eject the whole disc — but
/// a directory with nothing playable in it is an error.
pub fn load(dir: &Path) -> Result<Disc> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }

    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_audio(p))
        .collect();
    paths.sort();

    if paths.is_empty() {
        bail!("no audio files in {}", dir.display());
    }

    let mut tracks = Vec::new();
    let mut album: Option<String> = None;

    for path in paths {
        let Ok(p) = probe_file(&path) else { continue };
        if album.is_none() {
            album = p.album.clone();
        }
        tracks.push((
            p.track_no,
            Track {
                title: p.title.unwrap_or_else(|| {
                    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
                }),
                artist: p.artist.unwrap_or_else(|| "—".into()),
                seconds: p.seconds,
                path,
            },
        ));
    }

    if tracks.is_empty() {
        bail!("nothing in {} could be decoded", dir.display());
    }

    // If every track carries a track number, trust it over filename order —
    // that is the disc's own TOC. Otherwise filename order stands.
    if tracks.iter().all(|(n, _)| n.is_some()) {
        tracks.sort_by_key(|(n, _)| n.unwrap());
    }

    Ok(Disc {
        title: album.unwrap_or_else(|| {
            dir.file_name().unwrap_or_default().to_string_lossy().into_owned()
        }),
        tracks: tracks.into_iter().map(|(_, t)| t).collect(),
        path: dir.to_path_buf(),
    })
}

/// Find something to put in the tray at startup: the shallowest directory
/// under `root` that holds audio files.
///
/// Real libraries nest — `Music/Album/Disc 1/FLAC/*.flac` is ordinary — so this
/// walks rather than checking one level. Shallowest-then-alphabetical keeps the
/// choice predictable instead of depending on directory iteration order.
pub fn first_disc(root: &Path) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }

    let mut candidates: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .max_depth(6)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && is_audio(e.path()))
        .filter_map(|e| e.path().parent().map(Path::to_path_buf))
        .collect();

    candidates.sort();
    candidates.dedup();
    candidates.sort_by_key(|p| (p.components().count(), p.clone()));
    candidates.into_iter().next()
}
