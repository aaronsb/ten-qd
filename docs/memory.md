# The 12-volt memory

A car head unit has two power feeds. One is switched by the ignition; the other
runs permanently to the battery, and it exists for exactly one reason — so that
presets, tone settings and the last station survive the key coming out. Pull
that fuse and the radio comes back factory-blank.

This is that fuse.

## Where

```
$XDG_STATE_HOME/ten-qd/memory.toml      (default ~/.local/state/ten-qd/memory.toml)
```

**State, not config.** Nothing in the file is hand-authored — it is all
captured from the panel as you operate it, and losing it costs you nothing but
your presets. The XDG spec puts "recently used files" and view state in exactly
that category. It is readable TOML all the same, so editing presets by hand
works if you want to.

```toml
# ten-qd — the 12-volt memory.
# Written by the panel as you operate it. Delete this file, or run
# `ten-qd --forget`, to return the unit to factory-blank.

version = 1
source = "tuner"
volume = 0.55
bass = 0
treble = 0
fader = 0.5
ill = "green"
eq_front = [0.0, 3.0, 6.0, 3.0, 0.0, -3.0, 0.0, 6.0, 9.0]
eq_rear = [6.0, 3.0, 0.0, 0.0, -3.0, -6.0, -3.0, 0.0, 3.0]
tuner_freq = 103.7
tuner_presets = [89.0, 93.5, 0.0, 0.0, 0.0, 98.0]
disc = "/home/you/Music/some album"
browser = "/home/you/Music"
```

## What persists

| | |
|---|---|
| tuner | last station, the six presets, LOCAL |
| equaliser | both nine-band curves, the output trim |
| control head | volume, ATT, bass, treble, fader, illumination colour |
| every unit | whether it is in the signal path, as one `powered_off` list |
| transports | repeat, random, Dolby, auto-reverse |
| the deck | whether REC is switched on — always-append only works if it survives being switched off |
| loaded media | the disc in the tray and the tape in the deck, by path |
| browser | the folder you were last looking in |
| the rack | which units are in it, in what order, and which are folded shut |

The last of those is three lists of unit tokens — `cd`, `tape`, `tuner`, `eq`,
`amp`, `ctrl` — kept flat so the file stays worth editing by hand:

```toml
layout_order = ["tuner", "cd", "tape", "eq", "amp", "ctrl"]
layout_hidden = ["amp"]
layout_collapsed = ["cd"]
```

It is read defensively, because a hand-edited file is a file someone can get
wrong: unknown tokens are ignored, a unit named twice is placed once, and
anything left out is put back where the factory had it — not appended,
so a unit added in a later version arrives among its neighbours rather than at
the bottom of everyone's rack. A malformed arrangement costs
you the arrangement, never the ability to start.

## What does not

**Playback position, and the transport state.** The memory restores before the
first frame is drawn, so the panel never appears in a state you did not leave
it in — but the transport comes up stopped. A terminal program that starts
making noise the moment you launch it is a bad neighbour, whatever the car
would have done when you turned the key.

**Anything from `--screenshot`.** That is a diagnostic, and a diagnostic that
rewrote your settings would be a trap.

## How it is written

Nothing marks the memory dirty. Each frame the current settings are gathered
into a `Memory` and compared against what was last written; if they differ and
three seconds have passed, it is flushed, plus one unconditional write on the
way out.

The comparison rather than a dirty flag is deliberate: **a control added later
cannot be silently forgotten.** If it is a field in `Memory` it persists, and
if it is not, it does not — there is no third state where someone forgot to
call `mark_dirty()`.

Writes go to `memory.toml.tmp` and are renamed into place, so an interrupted
write leaves the previous memory intact rather than a truncated one.

## How it fails

Everything about reading it is designed around the file being disposable:

| what happened | what you get |
|---|---|
| file missing | factory-blank, silently — that is a first run |
| unreadable (permissions, I/O) | factory-blank, reported in the colophon |
| corrupt or truncated TOML | factory-blank, reported, **and rewritten** |
| written by a different `version` | factory-blank, reported, **and rewritten** |
| valid but with values out of range | clamped into range, kept |
| valid but missing fields | those fields take their defaults, rest kept |

The rewrite matters. The keeper only writes when the settings differ from what
is on disk, so a corrupt file that happens to decode to nothing would otherwise
sit there and greet you with the same complaint every launch. A damaged memory
carries a flag that forces exactly one write, and heals itself.

Nothing here is an error the program stops for. The worst case is a panel that
comes up factory-blank and says why.

### Values are clamped, not trusted

The file is user-editable and may be from an older build, so nothing read from
it is assumed to be in range:

```toml
volume = 9.5          ->  1.0
bass = 100            ->  2
tuner_freq = 5000.0   ->  108.0
eq_front = [99.0]     ->  [12.0, 0, 0, 0, 0, 0, 0, 0, 0]   (clamped and padded)
tuner_presets = [1.0] ->  slot 1 empty   (1 MHz is not an FM station)
```

A bad value should give you a flat equaliser, not a panic.

## Clearing it

```sh
ten-qd --forget      # or: make forget
```

`make uninstall` deliberately leaves the memory alone — removing a binary
should not throw away your presets. `make forget` is the separate, explicit
action.

## A note on testing

`Keeper` is told where to write rather than asking the environment for it.
That is not incidental: an earlier draft had it call `Memory::path()` itself,
and the first run of the self-healing test wrote a file into the developer's
real `~/.local/state`. Passing the path in makes the write target the same
thing the tests exercise, and makes it impossible for a test to reach the
user's actual memory.
