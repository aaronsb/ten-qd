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
use crate::state::{Tape, Track};

pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    /// Audio files directly inside this directory — what loading it as a disc
    /// would give you.
    pub here: usize,
    /// Audio files anywhere below it — what loading it as a tape would give.
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
    pub fn new(root: PathBuf) -> Self {
        let mut b = Browser {
            cwd: root.clone(),
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
                    here: count_here(&path),
                    below: count_below(&path),
                    name,
                    path,
                }
            })
            .collect();

        // The directory you are standing in is itself loadable when it holds
        // audio, so it gets a row at the top rather than forcing a step back.
        let here = count_here(&self.cwd);
        if here > 0 {
            self.entries.insert(
                0,
                Entry {
                    path: self.cwd.clone(),
                    name: ". (this folder)".into(),
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

    /// Descend into the highlighted directory.
    pub fn enter(&mut self) {
        let Some(e) = self.selected() else { return };
        if e.path == self.cwd {
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

    /// Gather every track below the highlighted directory into a tape.
    ///
    /// Sorted by path so a multi-disc set stays in its intended order, then
    /// split into two sides by running time.
    pub fn as_tape(&self) -> Result<Tape> {
        let Some(e) = self.selected() else { bail!("nothing selected") };

        let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(&e.path)
            .max_depth(SCAN_DEPTH)
            .follow_links(false)
            .into_iter()
            .filter_map(|x| x.ok())
            .filter(|x| x.file_type().is_file() && disc::is_audio(x.path()))
            .map(|x| x.path().to_path_buf())
            .collect();
        paths.sort();

        if paths.is_empty() {
            bail!("no audio under {}", e.name);
        }

        let tracks: Vec<Track> = paths.iter().filter_map(|p| disc::probe_track(p).ok()).collect();
        if tracks.is_empty() {
            bail!("nothing under {} could be decoded", e.name);
        }

        Ok(Tape::from_tracks(e.name.clone(), tracks))
    }
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}
