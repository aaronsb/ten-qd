# The rack is an output device

You do not route your speakers. You point things at them.

That sentence is the whole of this document, and everything below is the
argument for why the current design cannot honour it and what has to change so
that it can. **Nothing here is built yet.** This is a decision, written down
before the work, because the reasoning cost two sessions of diagnosis to arrive
at and would be expensive to reconstruct from the code that resulted.

## What is wrong now

The rack plays audio by opening an ALSA PCM through cpal. PipeWire sees the
result and creates a node for it:

```
node.name      = alsa_playback.ten-qd
media.class    = Stream/Output/Audio
client.api     = alsa
```

Two of those three lines are fatal to owning your own routing.

**`Stream/Output/Audio` means "an application playing a sound."** That is, by
definition, the category the desktop's routing policy exists to move around.
Every policy agent on the system — WirePlumber, EasyEffects, the sound settings
panel — is entitled to decide where that stream goes, and correct to do so. The
rack is not being singled out; it is being treated exactly as what it declared
itself to be.

**`client.api = alsa` means the node is not ours.** cpal opens a PCM and
*pipewire-alsa* creates the node on our behalf. We never touch the graph, so we
cannot declare `target.object` or `node.dont-reconnect` or any of the properties
that would express an intention about where our audio goes. The only lever left
is `pactl move-sink-input` — a one-shot advisory request with no memory, which
any policy agent may undo a moment later, and which several will.

The [LINK indicator](sources.md#aux-is-a-wire) exists because of this. It is a
good indicator and it was worth building — a panel that cannot say when its own
claims have stopped being true is worse than no panel. But it is a *smoke
detector*, and this document is about the wiring.

## What that looked like in the field

EasyEffects with "process all output streams" enabled re-homed every stream on
the machine, including ours, about once a second. The resulting graph:

```
ten-qd ──▶ easyeffects_sink ──▶ EasyEffects' filter chain ──▶ ten_qd_aux ──▶ ten-qd's own capture
```

Four hops, a complete circle, and **zero links to the actual headphones** —
while the panel and the 12-volt memory both cheerfully read
`output = "Muh Chickin Waffles"`.

The rack was not at fault, and proving that took moving *Spotify's* stream
instead and watching it snap back identically. Nothing about ten-qd provoked
this. It is what being a `Stream/Output/Audio` means.

Two lessons worth keeping separately from the fix:

- **The loop detector could not see it.** `own_output_is_looping` compared our
  sink-input's sink index against our own sink's index — one hop, for a question
  that spanned four. It answered "no loop" while audio went in circles.
- **It could not have been fixed on `pactl`.** PulseAudio's object model has no
  filter nodes, so the offending edge was invisible to that tool *in principle*.
  When a predicate keeps being wrong, check whether its data source can express
  the right answer at all.

## The design

Register as an `Audio/Sink` — a device the desktop lists in its output picker
alongside the headphones and the speakers — with the rack's DSP as filter nodes
and *explicit* port links to hardware. This is EasyEffects' own architecture,
which is a point in its favour: the pattern is well-trodden and the thing that
kept beating us is the proof that it works.

The operator selects "ten-qd" as their output once, and every application
follows, because that is what selecting an output device means. No grabbing, no
timer, no fight.

What falls out of it:

- **The output stops being a sink-input.** There is nothing left for a policy
  agent to move, so the whole class of failure becomes structurally impossible
  rather than merely detected.
- **The feedback loop cannot be formed.** Not guarded against — unformable.
- **AUX goes back to being a scalpel.** "Plug this one application through the
  rack" is a fine feature and should stay. It is currently carrying the entire
  design, which is more weight than a convenience feature can bear.

## What it cannot do

It cannot outrank a program with equal privileges that deliberately and
repeatedly rewires it. **Nothing achieves that**, and it is important not to
promise it. A determined EasyEffects can still be pointed at us, or us at it.

The claim is narrower and worth more: being the wrong *kind of object* was the
actual problem. A sink is not a thing the routing policy moves, so the ordinary
case — a policy agent doing its job correctly — stops producing a wrong answer.

## What it costs

cpal comes out; pipewire-rs goes in. The engine's callback and its ring buffers
survive unchanged — what changes is who calls the callback. This makes the build
PipeWire-only, which sounds like a loss and is not: it already shells out to
`pactl` and `pw-record`, so the portability was theoretical.

## The graph walk, folded in

One piece of the diagnosis is worth building regardless, because it answers a
question the panel currently gets wrong in two places.

Plain reachability, one breadth-first search, no weights — **not** Dijkstra;
there are no costs here, only edges. One traversal answers "does my output reach
hardware," and run backwards from the input sink it answers "is anything
actually feeding my ear," which the AUX bay currently decides one hop at a time
and therefore sometimes decides wrongly.

Read `pw-dump`, never `pactl` — see the second lesson above for why the data
source is the load-bearing part of that sentence.

A real broken-state `pw-dump` was captured during the diagnosis and belongs in
the repository as a fixture before this work starts. Reproducing that graph on
purpose is difficult; losing it would be careless.
