//! The disc/tape browser.
//!
//! A shelf, not a file manager: it lists directories and tells you how much
//! audio is in each, because the only question being asked is "what do I put
//! in the machine". Two ways to answer it —
//!
//! - **as a disc**: the folder's own audio files, in TOC order. Flat, because
//!   a disc is one physical object.
//! - **as a tape**: everything below the folder, recursively, compiled into a
//!   playlist and split into two sides. That is what a tape *was* — a
//!   selection gathered from several sources onto one piece of media.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::disc;
use crate::playlist;
use crate::state::{Tape, Track};

/// What a row in the shelf is.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    /// A directory: loadable as a disc (its own files) or a tape (everything
    /// below it).
    Folder,
    /// An M3U or PLS file: a tape someone already compiled.
    Playlist,
}

pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    /// Audio files directly inside this directory — what loading it as a disc
    /// would give you. Always zero for a playlist, which is not a disc.
    pub here: usize,
    /// Tracks a tape load would give: files below a folder, or lines in a
    /// playlist.
    pub below: usize,
}

impl Entry {
    pub fn playable(&self) -> bool {
        self.below > 0
    }
}

pub struct Browser {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    pub open: bool,
    /// Reported back to the panel when a load fails.
    pub error: Option<String>,
}

/// How deep to look when counting audio below a directory. Deep enough for
/// `Album/Disc 1/FLAC`, shallow enough not to walk a whole filesystem.
const SCAN_DEPTH: usize = 5;

fn count_below(dir: &Path) -> usize {
    walkdir::WalkDir::new(dir)
        .max_depth(SCAN_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && disc::is_audio(e.path()))
        .count()
}

fn count_here(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_file() && disc::is_audio(&e.path()))
                .count()
        })
        .unwrap_or(0)
}

impl Browser {
    /// `start` is where the browser opens — the folder it was left in last
    /// time, if the memory has one and it still exists.
    pub fn new(root: PathBuf, start: Option<PathBuf>) -> Self {
        let cwd = start.filter(|p| p.is_dir() && p.starts_with(&root)).unwrap_or_else(|| root.clone());
        let mut b = Browser {
            cwd,
            root,
            entries: Vec::new(),
            cursor: 0,
            open: false,
            error: None,
        };
        b.refresh();
        b
    }

    pub fn refresh(&mut self) {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&self.cwd)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && !is_hidden(p))
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();

        self.entries = dirs
            .into_iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                Entry {
                    kind: Kind::Folder,
                    here: count_here(&path),
                    below: count_below(&path),
                    name,
                    path,
                }
            })
            .collect();

        // Playlists sit below the folders: someone else already decided the
        // running order, which is exactly what a compiled tape is.
        let mut lists: Vec<PathBuf> = std::fs::read_dir(&self.cwd)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && playlist::is_playlist(p))
                    .collect()
            })
            .unwrap_or_default();
        lists.sort();

        for path in lists {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let below = playlist::read(&path).map(|l| l.entries.len()).unwrap_or(0);
            self.entries.push(Entry { kind: Kind::Playlist, here: 0, below, name, path });
        }

        // The directory you are standing in is itself loadable when it holds
        // audio, so it gets a row at the top rather than forcing a step back.
        let here = count_here(&self.cwd);
        if here > 0 {
            self.entries.insert(
                0,
                Entry {
                    path: self.cwd.clone(),
                    name: ". (this folder)".into(),
                    kind: Kind::Folder,
                    here,
                    below: count_below(&self.cwd),
                },
            );
        }

        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    pub fn move_by(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as i32;
        self.cursor = (self.cursor as i32 + delta).rem_euclid(n) as usize;
    }

    /// Descend into the highlighted directory. A playlist has nothing to
    /// descend into, so entering one loads it.
    pub fn enter(&mut self) {
        let Some(e) = self.selected() else { return };
        if e.path == self.cwd || e.kind == Kind::Playlist {
            return;
        }
        let path = e.path.clone();
        self.cwd = path;
        self.cursor = 0;
        self.refresh();
    }

    /// Step back towards the root, but never above it.
    pub fn parent(&mut self) {
        if self.cwd == self.root {
            return;
        }
        if let Some(p) = self.cwd.parent().map(Path::to_path_buf) {
            self.cwd = p;
            self.cursor = 0;
            self.refresh();
        }
    }

    /// Compile the highlighted row into a tape, whichever kind it is, along
    /// with how many lines it could not cue. A folder has none by definition.
    pub fn as_tape(&self) -> Result<(Tape, usize)> {
        let Some(e) = self.selected() else { bail!("nothing selected") };
        match e.kind {
            Kind::Folder => tape_from_dir(&e.path).map(|t| (t, 0)),
            Kind::Playlist => tape_from_playlist(&e.path),
        }
    }
}

/// Compile a playlist file into a tape, in the order the file gives.
///
/// Unlike a folder scan this does **not** sort: a playlist's whole point is
/// that someone already chose the running order.
pub fn tape_from_playlist(path: &Path) -> Result<(Tape, usize)> {
    let loaded = playlist::read(path)?;
    let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();

    let tracks: Vec<Track> = loaded
        .entries
        .iter()
        .filter_map(|e| {
            // Metadata in the file beats the playlist's label; the label is
            // only a fallback for a file that will not admit its own title.
            let mut t = disc::probe_track(&e.path).ok()?;
            if let Some(label) = &e.title
                && t.title == e.path.file_stem().unwrap_or_default().to_string_lossy()
            {
                t.title = label.clone();
            }
            Some(t)
        })
        .collect();

    if tracks.is_empty() {
        bail!("nothing in {name} could be decoded");
    }
    Ok((Tape::from_tracks(name, path.to_path_buf(), tracks), loaded.remote))
}

/// Gather every track below `dir` into a tape.
///
/// Sorted by path so a multi-disc set stays in its intended order, then split
/// into two sides by running time. Shared with the memory, which needs to put
/// the same tape back in the deck at start-up without going through the
/// browser to do it.
pub fn tape_from_dir(dir: &Path) -> Result<Tape> {
    let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();

    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .max_depth(SCAN_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_map(|x| x.ok())
        .filter(|x| x.file_type().is_file() && disc::is_audio(x.path()))
        .map(|x| x.path().to_path_buf())
        .collect();
    paths.sort();

    if paths.is_empty() {
        bail!("no audio under {name}");
    }

    let tracks: Vec<Track> = paths.iter().filter_map(|p| disc::probe_track(p).ok()).collect();
    if tracks.is_empty() {
        bail!("nothing under {name} could be decoded");
    }

    Ok(Tape::from_tracks(name, dir.to_path_buf(), tracks))
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}
