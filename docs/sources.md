# The three sources

One source at a time — this is a head unit, not a mixer. Everything downstream
of the source is identical no matter where the audio came from, because the
decoder thread is the only producer on the output ring. For the tuner its job
is simply to pump the radio's ring into the output ring.

## A disc is a folder

`disc::load` opens **every** file in the folder before reporting a disc. That
is deliberate and it is why loading a large album takes a moment: the display
cannot honestly show a track count or a total time it has not verified. A real
player reads the lead-in before it plays a note.

Ordering follows the disc's own table of contents — if every file carries a
track number, that wins; otherwise filename order stands. A file that will not
open is skipped rather than aborting the load, because one bad track should not
eject the whole disc.

The display shows a track number, an elapsed time, and a music calendar. That
is all a 1985 player had, and the discipline is the point. Track titles live on
the shelf strip *below* the panel, off the face entirely.

The **music calendar** is the grid of track numbers: printed if the disc has
that track, lit while it is playing. Twenty cells and an OVER lamp, which is
how real players handled discs with more tracks than the calendar had room for.
Clicking a cell cues that track.

## A tape is a playlist

And its two sides are the two halves of that playlist.

`Tape::from_tracks` splits where the **cumulative running time** crosses the
midpoint — not at the midpoint of the track count. A cassette holds a fixed
running time per side, so a long opener goes on side A alone:

```rust
// 10:00, 1:00, 1:00, 1:00  ->  side A: 1 track, side B: 3 tracks
```

That is the same arithmetic anyone compiling a tape by hand used to do.

### Why the deck earns a bay next to the CD player

Because of what it *cannot* show. A cassette has no index, so the QD-581
displays a **linear four-digit counter** that resets when you turn the tape
over — not a track number, not a track time. Everything the CD player states
precisely, the deck can only approximate.

Two units, one file decoder, completely different character. That difference is
the reason both exist.

- **Auto-reverse** flips at the end of side A. Coming back to side A only
  happens on repeat; otherwise the deck stops at the end of side B, as it would.
- **APS** — Automatic Program Search — is the deck's name for track skip. Below
  three seconds into a track it steps back; past that it restarts the current
  one, the same rule the CD player follows.
- **REW / FF** wind by ten seconds a press, seeking within the track. A deck has
  no index to jump to, so this scrubs rather than skipping.
- **FLIP** turns the cassette over and restarts the counter.

## A station is a station

See [tuner.md](tuner.md).

## The browser

`o` opens a shelf, not a file manager. The only question being asked is *what
do I put in the machine*, and there are two ways to answer it:

| | |
|---|---|
| **`d` — load as disc** | the folder's own audio files, in TOC order. Flat, because a disc is one physical object. |
| **`t` — load as tape** | everything below the folder, recursively, compiled into a playlist and split into two sides. |

Each row shows both counts, so the distinction is visible before you commit:

![The browser open over a running disc](img/rack-green-browser.png)

```
  Satisfactory Soundtrack FLAC                 0 disc    48 tape
```

Nothing sits directly in that folder — 48 files are below it. Loading it as a
disc would give you an empty tray; as a tape it gives you a 48-track
compilation across two sides. Real libraries nest, so the browser walks up to
five levels rather than checking one.

## Switching sources

Selecting a source stops whatever the others were doing, the way a
single-transport head unit has to. Each source remembers its own position, so
switching away and back returns you to where you were.

The shared transport keys — `SPACE`, `s`, `←`, `→` — act on whichever source is
selected. The digit keys are context-sensitive: track cue on the disc and the
tape, preset recall on the tuner.
