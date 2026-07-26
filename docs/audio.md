# The signal path

```
decode ──▶ tone shelves ──┬──▶ FRONT bank: 9 peaking biquads ──┐
(symphonia)               │                                    ├─▶ fader ─▶ volume ─▶ out
                          └──▶ REAR  bank: 9 peaking biquads ──┘                       (cpal)
```

```
decoder thread ──push──▶ [ring] ──pop──▶ cpal callback ──▶ device
 symphonia                                  │  DSP
 resample                                   └──push──▶ [ring] ──▶ analysis thread
                                                                    9-band FFT
```

## The equaliser equalises

Eighteen sliders are eighteen RBJ peaking biquads across two independently
curved buses. Two banks of nine is what the advertisement's two rows of caps
actually mean, and here they are two real filter banks.

Bands sit roughly an octave apart (60 Hz … 16 kHz), so `Q = 1.41` gives skirts
that meet without the comb-filtering you get from stacking narrow bells. A band
sitting at 0 dB is tuned to bypass rather than to a unity-gain filter, so a flat
curve costs nothing.

`boosting_a_band_raises_that_band` in `dsp.rs` feeds a 1 kHz sine through the
chain and asserts the output level actually moves. That test is what stops the
equaliser from quietly becoming decoration during some future refactor.

## The meters measure

The QM-571's nine columns are a 9-band FFT of the **post-DSP** output, bound to
the same centre frequencies the equaliser controls and drawn in the same column
positions on screen. Pull the 250 Hz slider down and the third meter column
drops.

This is a deliberate departure from the advertisement, where the meters run
horizontally. It is worth the departure: it makes the rack legible as one
instrument rather than two panels that happen to be stacked. The column
alignment is enforced by a single `BAND_X` constant shared by both units —
moving it moves both panels together.

Ballistics are fast attack, slow release. A meter that falls as fast as it
rises reads as noise; this reads as music. Peak-hold puts a solid red bar on
top of each lit column, which is what the ad's meters actually show — amber
dots with a red peak bar, not a green-to-red ramp.

The FFT runs on its own thread, fed from the callback by a ring buffer. Nothing
in `analysis.rs` executes inside the audio callback.

## The clock

The elapsed-time display counts frames the **output callback actually
delivered**, not the decoder's read position. The decoder runs ahead by the
depth of the ring, so using its position would make the display lead the sound.

Track changes are stamped into the stream: the decoder records the total-frames
value at each track start (`TrackMark`) and the UI promotes the display when
the delivered count passes the stamp. That stays correct through gapless joins,
which is what lets a tape side play as one piece.

Only frames that actually had audio advance the clock, so an underrun shows as
the display pausing rather than as accumulated drift.

### Flushing

Anything that discontinuously changes the stream — stop, cue, eject, flip, seek
— invalidates those stamps. Those go through a handshake: the decoder stops
pushing, asks the callback to drain the ring and zero the counter, waits for the
acknowledgement, and only then resumes. Producer and consumer are never both
touching the ring during a reset.

Commands that flush also carry an **epoch**. The UI increments it, the decoder
stamps every mark with the epoch in force, and the UI discards marks from a
superseded epoch. Without it, a mark emitted just before a STOP arrives just
after it, and the display jumps to a track that is no longer playing.

## Parameters

Everything the callback needs is published as one immutable `DspParams`
snapshot through an `ArcSwap`. The UI thread swaps a whole new one in; the audio
thread never blocks.

A **generation** counter separates changes that need new filter coefficients
(equaliser, tone, defeat) from continuous ones that do not (volume, fader, ATT,
power). Only the former trigger a retune; the latter are read fresh every
block. Output gain is smoothed with a one-pole at about 5 ms, so a volume press
does not click.

## Resampling

The device usually runs at 48 kHz and discs are 44.1 kHz, so the resampler is
in the path constantly. It is 4-point third-order Hermite — not a windowed
sinc. Cheap, stateless enough to run inline, and it trades a little
high-frequency accuracy for that.

`rubato` is the drop-in upgrade and `hermite()` in `audio/mod.rs` is the only
function that would change. Three tests guard it: passthrough is bit-exact at
matched rates, the output length matches the rate ratio, and a DC signal
survives unchanged — that last one catches sign and coefficient errors in the
kernel.

## The fader

The two banks are summed by the fader rather than driving four speakers,
because the output device is stereo. Moving the fader audibly crossfades
between the two curves. It is the honest stereo reduction of a four-channel car
rig, and it is not the same thing as having four channels.

The crossfade is equal-power, so moving the fader does not dip the total level
through the middle.
