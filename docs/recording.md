# Recording

*A design note. None of this is built yet.*

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
| **TRACK** | an M3U playlist | the sequence — what played, in order, with its source | anything without a title. A microphone contributes nothing. |
| **AUDIO** | a file per take | the signal, exactly as it was | which parts of it were which. |

The modes are not two settings of one feature. They are two different machines
that happen to share a button.

**TRACK is what a mixtape actually was.** Nobody compiling one in 1986 thought
they were manufacturing music; they were recording an *order* — this song, then
this one, because of how the second lands after the first. That is the artefact.
The audio was only ever the storage medium it had to be written on.

So TRACK writes down the order and nothing else. It costs no disk, duplicates
no content, and produces a tape that loads straight back into the deck — which
replays it by driving the player over MPRIS. Same songs, same order, no copy.
`playlist.rs` already reads and writes M3U with `#EXTINF`; `Mpris::poll` already
runs at 2 Hz and reports title, artist and album. A change in that tuple is a
track boundary.

**AUDIO is for the things that have no playlist.** A microphone, a line input,
a broadcast. Those exist only as signal, and a playlist cannot represent them at
all — which is the mode saying something true about its own limits rather than a
gap to paper over.

### Honest about the jumble

Play Spotify, a YouTube tab and a microphone at once and the two modes answer
differently, both correctly:

- AUDIO records the sum, indistinguishable, because that is what it was. The
  aux sink is already a mix bus — PipeWire sums whatever is moved onto it.
- TRACK records three interleaved entries with their players attached, and says
  nothing about the microphone, because a microphone has no title.

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
today, which is right for driving transport keys and wrong for recording a mix.

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
