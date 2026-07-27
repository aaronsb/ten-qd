# Recording

*TRACK mode and the projection that cuts a playlist out of the log are built.
AUDIO mode is not — the sections below are marked where they describe something
that does not exist yet.*

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
| **TRACK** | the listening log | the sequence — what played, in order, with its source | what a nameless signal *is*. It can still say where it came from. |
| **AUDIO** *(not built)* | a file per take | the signal, exactly as it was | which parts of it were which. |

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

**As built:** `R`, or the ●REC key on the deck, switches it on, and the switch
is kept in the 12-volt memory — always-append only gathers a week of listening
if it survives the key coming out. There is no mode selector yet because there
is only one mode; AUDIO will need one.

REC is a switch, and the lamp beside it is a readout. It lights only while
entries can actually be appended — take the deck out of the signal path, or let
a write fail, and the lamp goes dark (`NO LOG` for the second case) while the
switch stays where you left it. Pull the power and put it back and the log
picks up.

The window reports two counts, both of things that already happened: entries
committed to disk, and players whose entry has already run long enough that it
*will* be written. The second is filtered by the same five-second rule the
writer uses, which costs a new track five seconds before it appears and buys
the count meaning exactly what it says.

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

**As built:**

```
  ten-qd --log                     what the log holds, by session
  ten-qd --export                  everything, in the order first heard
  ten-qd --export=last             the session that just ended
  ten-qd --export=2026-07 --rank   a month, ordered by play count
```

The playlist goes to stdout so it can be redirected; everything *about* the
playlist goes to stderr so that redirect stays clean. A selector is matched on
its digits alone, so `2026-07`, `202607` and a full session id are all the same
question asked three ways.

Three decisions the projection makes, none of which the log is allowed to:

- **Repeats collapse, and the count survives.** `#PLAYS:` precedes anything
  heard more than once. The log keeps every play because how often you played
  something is the signal; the playlist is where that becomes a ranking.
- **A track's `#EXTINF` is the longest play seen.** The log records how long
  *you played* something, which for a song heard five times is five different
  numbers. The longest is the closest anyone can get to its real length from
  this side, and length is what splits a tape into sides.
- **A track with nowhere to point is a comment.** A browser tab routinely
  reports a title and no location. Dropping those would lose an afternoon;
  giving them an `#EXTINF` would hand their title to whatever line came next,
  because that is how the format works. So they come out named, counted, and
  plainly marked as unplayable.

### Noticing what is playing

Appending only works if the deck notices sources arriving and leaving on their
own. `mpris.rs` polls the whole bus at 2 Hz and keeps every player; the aux
bay's single "what is on the cable" is now a choice made out of that list
rather than a separate scan.

A new entry begins when a player that is playing has something to say, and ends
when its `(artist, title, album)` tuple changes or the player goes away — either
way, with however long it actually ran. Players are keyed by bus name, because
two Chromium windows both call themselves the same thing and one of them
starting a second video is not the first one changing track.

Three rules keep the log from filling with things nobody listened to. Time is
only credited while a player reports itself playing, so a track left paused
overnight records the minute you heard rather than the eight hours it sat
there. Anything under five seconds is dropped: scrubbing through an album is
not twelve plays. And no single poll may credit more than a few seconds —
because time is credited in arrears, and closing the lid at 22:00 with Spotify
playing otherwise makes the first poll of the morning book a ten-hour listen
into a file that is never rewritten.

Two silences are also read as silences rather than as events. A player still
present but reporting no metadata — one failed D-Bus call — is not a track
change, or a hiccup would split one play into two, doubling its count and
halving the length every tape cut from it inherits. And a bus that cannot be
reached at all is not a bus with nothing on it, so an outage holds the open
entries rather than closing every one of them.

So TRACK writes down the order and nothing else. It costs no disk and
duplicates no content, and the tape cut out of it later loads straight back
into the deck — which replays it by driving the player over MPRIS. Same songs,
same order, no copy.

**AUDIO is for the things that have no playlist.** *(Not built.)* A microphone, a line input,
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

## Consequences still to settle

**REC LEVEL is its own gain stage.** GAIN is the equaliser's *output* trim,
downstream of the tap, so it cannot serve. That is correct — a deck's REC LEVEL
was always separate from its playback volume — but it is a second gain control
and the two must be visibly different things or people will reach for the wrong
one.

**REC PAUSE is the preview.** Arm the deck, meters live, tape stationary; set
the level against the meter; release. Three states, not two — idle, armed,
running — and worth building in from the start rather than retrofitting. The
input meter is already a record-level meter; that is what those are.

**A track's duration is what you played, not what it was.** *Settled:* the log
counts wall-clock seconds while the player says it is playing, rather than
reading the track length off MPRIS. Skip halfway and it says so — which matters
downstream, because sides are split by running time.

**A service URI is not a path.** *Half settled:* `playlist.rs` now recognises a
URI scheme rather than only `://`, so `spotify:track:…` is turned away instead
of being read as a filename — and a tape made entirely of them says how many it
could not cue rather than "lists nothing playable". What is *not* settled is
replay: driving the service over MPRIS to play its own track back is a feature
that does not exist, so for now such a tape is a list you can open elsewhere
rather than one the deck can run.

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
