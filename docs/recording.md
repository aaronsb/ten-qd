# Recording

*A design note. None of this is built yet.*

**In one line:** the deck keeps an append-only log of everything you listen to,
across every service, and a playlist is something you cut out of it later.

## The machine does not decide

Old equipment did not make decisions about what was allowed. A tape deck had a
RECORD button and two input jacks; it did not know what you were recording, did
not ask, and could not have refused. Neither could the radio, the phono stage
or the amplifier. Every restriction lived with the person operating the
equipment — in law, in contracts, in manners — and none of it lived in the box.

That is not nostalgia. It is a different distribution of authority, and it is
the thing modern audio software has quietly taken back. A streaming client
decides what you may do with a stream. The rack, by design, does not.

The important part is that this makes the machine's obligations *heavier*, not
lighter. If it will not judge for you, it owes you an unusually honest account
of what it is doing so that you can. That is the same rule the panel already
lives by, pointed at the tape:

> **The deck's job is to tell the truth about the signal. Yours is to decide
> what to do with it.**

Two things follow that might look like exceptions and are not.

**Truthfulness is not a restriction.** Refusing to stamp a recording with a
source it did not come from is not the machine policing you; it is the machine
declining to lie to you. A deck that labelled your microphone take with someone
else's metadata would be broken, not permissive.

**The analogue hole is not circumvention.** Recording what is already coming
out of the speakers is what every tape deck ever made did. Breaking a stream's
protection to get a cleaner copy is a different act with a different name. The
rack sits on the near side of that line by construction — it records the mix
bus, which is holding a microphone to a speaker without the room in the way.

## Two modes, with complementary blind spots

| mode | writes | captures | cannot know |
|---|---|---|---|
| **TRACK** | an M3U playlist | the sequence — what played, in order, with its source | what a nameless signal *is*. It can still say where it came from. |
| **AUDIO** | a file per take | the signal, exactly as it was | which parts of it were which. |

The modes are not two settings of one feature. They are two different machines
that happen to share a button.

**TRACK is what a mixtape actually was.** Nobody compiling one in 1986 thought
they were manufacturing music; they were recording an *order* — this song, then
this one, because of how the second lands after the first. That is the artefact.
The audio was only ever the storage medium it had to be written on.

### It is a log, not a tape

The obvious shape is "press REC, get a tape". The better shape is **always
appending**: the deck keeps a running record of what you listened to, and a
*tape* is something you cut out of it afterwards.

That falls out of what the mode is actually for. Play YouTube Music in a tab in
the morning and Spotify in the afternoon, and what you want at the end of the
week is not two tapes — it is one list of the things you liked, gathered across
services that have no interest in letting you hold that list. TRACK mode is a
way of getting your own listening back out of them, and appending is what makes
that work without you having to decide in advance that a moment was worth
keeping.

So the mode has two settings, both persisted in the 12-volt memory:

| | |
|---|---|
| **record mode** | TRACK or AUDIO |
| **always append** | keep logging whenever the rack is running |

### The log, and what is cut from it

The native format is ours, append-only, and richer than a playlist:

```
  ~/.local/state/ten-qd/listening.jsonl
```

One JSON object per line, appended and never rewritten. That choice is
load-bearing rather than aesthetic: appending a line is a single write that
cannot corrupt what came before, a torn final line after a crash is discardable
on its own, and the whole thing stays greppable. A TOML or XML log would have to
be re-serialised on every track change — a rewrite of the entire history, several
times an hour, for a program that should survive being killed.

Each entry carries what a playlist cannot:

```json
{"at":"2026-07-26T21:55:56Z","session":"...","player":"Spotify",
 "artist":"Boards of Canada","title":"Age Of Capricorn",
 "album":"Tomorrow's Harvest","seconds":214,"uri":"spotify:track:..."}
```

**Sessions** group entries into contiguous listening. A session is a run of the
rack, so "that Tuesday evening" is a thing you can name and select — and
selecting a set of sessions and projecting them into M3U or PLS is how a tape
gets made. The log is the archive; a playlist is a *view* of it.

That projection is the point where this stops being a music player feature and
becomes genuinely useful: pick a month, dedupe, rank by play count, and you have
your actual taste across every service you used, in a format any player can
open. Nothing about that requires the services to cooperate, and none of them
offer it.

### Noticing what is playing

Appending only works if the deck notices sources arriving and leaving on their
own. `mpris.rs` polls at 2 Hz and picks *one* player; TRACK mode needs
`find_all()` and needs to treat players as coming and going — Chromium at
breakfast, Spotify after lunch, both at once while one is paused.

A new entry is warranted when a player's `(artist, title, album)` tuple changes,
or when a player appears already playing something. A player disappearing closes
its current entry with however long it actually ran.

So TRACK writes down the order and nothing else. It costs no disk, duplicates
no content, and produces a tape that loads straight back into the deck — which
replays it by driving the player over MPRIS. Same songs, same order, no copy.
`playlist.rs` already reads and writes M3U with `#EXTINF`; `Mpris::poll` already
runs at 2 Hz and reports title, artist and album. A change in that tuple is a
track boundary.

**AUDIO is for the things that have no playlist.** A microphone, a line input,
a broadcast. Those exist only as signal, and no playlist can carry them.

But they are not anonymous. PipeWire names every device, and the names are
better than anything we would invent:

```
  OBSBOT Meet 2 Analog Stereo
  RemoteSL + ZeroSL Analog Stereo
  Maonocaster E2 Analog Stereo
```

That is real provenance, and it arrives in the same `description` field
`adapter::sinks()` already reads — a `Source { name, description }` twin of the
existing `Sink`, from `pactl -f json list sources`.

So TRACK's blind spot is narrower than "a microphone contributes nothing". It
cannot say *what* a nameless signal is, because nothing knows; it can say
exactly where it came from and how long it ran:

```
  #EXTINF:214,OBSBOT Meet 2 Analog Stereo
```

An entry that names its input and admits it has no title is honest. Omitting it
would be the tape quietly losing four minutes of your afternoon.

### Honest about the jumble

Play Spotify, a YouTube tab and a microphone at once and the two modes answer
differently, both correctly:

- AUDIO records the sum, indistinguishable, because that is what it was. The
  aux sink is already a mix bus — PipeWire sums whatever is moved onto it.
- TRACK records three interleaved entries with their players attached, plus one
  for the microphone naming the device rather than a title — because that is
  everything anyone knows about it.

Neither mode pretends to the other's knowledge.

## Where the tape taps the signal

**Before the equaliser** — which in this chain also means before the volume.

```text
  cons.pop_slice(&mut stereo)   ← the tap
  chain.process(&mut stereo)      tone · 18 EQ biquads · fader · GAIN · clip
```

One insertion point in the output callback, ahead of everything, and *after* the
source multiplexer — so it works identically for CD, tape, tuner and aux without
knowing which is selected.

This is how real decks were wired, and the reason is practical: **record level
is independent of listening level.** Turn the volume to zero and the recording
is unaffected. You can record at three in the morning with nothing coming out of
the speakers. It also means a curve set for your headphones cannot be baked into
the tape — you would hear it a second time on playback, through whatever you
played it back on.

For AUDIO mode on aux there is a simpler alternative worth weighing: point
`pw-record` at the aux sink's monitor and let it write the file directly. That
is upstream of ten-qd entirely, needs no tap and no encoder, and captures
exactly what arrived. The callback tap is uniform across sources; the monitor
tap is simpler but aux-only.

## Consequences to settle before building

**REC LEVEL is its own gain stage.** GAIN is the equaliser's *output* trim,
downstream of the tap, so it cannot serve. That is correct — a deck's REC LEVEL
was always separate from its playback volume — but it is a second gain control
and the two must be visibly different things or people will reach for the wrong
one.

**REC PAUSE is the preview.** Arm the deck, meters live, tape stationary; set
the level against the meter; release. Three states, not two — idle, armed,
running — and worth building in from the start rather than retrofitting. The
input meter is already a record-level meter; that is what those are.

**TRACK mode needs `find_all()`, not `find()`.** `mpris.rs` picks one player
today, which is right for driving transport keys and wrong for logging
everything that plays.

**Projection needs a command.** Selecting sessions and emitting a playlist is a
separate verb from recording — likely `--export` with a session range and a
format, rather than anything on the panel. The deck records; cutting a tape out
of the log is a different job.

**Deduplication belongs to projection, not the log.** The log keeps every play,
because how often you played something is the signal. The playlist that comes
out of it collapses repeats.

**A track's duration is what you played, not what it was.** MPRIS reports
position. Skip halfway and the `#EXTINF` should say so, because sides are split
by running time and a tape should be honest about how long it actually ran.

**A service URI is not a path.** `playlist.rs` deliberately drops lines
containing `://` as "a station, not a track" — correct for playback, and now
needing a deliberate exception. Whether such a tape can *replay* depends on the
service, and the deck should say so rather than silently failing to cue.

**A microphone and speakers are an acoustic loop.** Unlike the routing loop, no
guard can prevent it. The deck can only warn.

## What the deck already has

Recording is the first feature that makes the cassette bay's existing furniture
mean something. None of this needs inventing:

| | |
|---|---|
| **COUNTER** | counts real elapsed tape, which is what it has always been for |
| **SIDE A/B** | splits a recording the way `Tape::from_tracks` already splits a folder |
| **MTL · DOLBY** | stop being lamps and become properties of what is being written |
| **the input meter** | is a record-level meter already |

And the loop closes: record from aux or the tuner, get a tape, put it in the
deck. The rack starts feeding itself.

## See also

- [sources.md](sources.md) — the aux input, and why the cable became a source
- [audio.md](audio.md) — the DSP chain the tap sits ahead of
