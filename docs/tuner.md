# The tuner

Broadcast FM, demodulated in-process from an RTL-SDR.

```
RTL2832 ─▶ u8 IQ @ 1.024 MHz ─▶ FIR ÷4 ─▶ 256 kHz ─▶ discriminator ─▶ MPX
                                                                      │
       ┌──────────────────────────────────────────────────────────────┤
       ▼                                                              ▼
19 kHz pilot ─▶ ×² ─▶ 38 kHz ─▶ × MPX ─▶ LPF ─▶ (L−R)      LPF 15 kHz ─▶ (L+R)
                                              └──────┬─────────────────┘
                                 de-emphasis 75 µs ─▶ L, R ─▶ resample ─▶ ring
```

## Why not `rtl_fm`

Shelling out to `rtl_fm` would have been an afternoon's work and would have
produced audio just as listenable. It was rejected because two indicators on
the panel depend on the demodulator being ours:

- **SIGNAL** reads the power of the tuned channel, taken off the IQ stream.
- **STEREO** lights only when a 19 kHz pilot is actually present in the
  multiplex.

Neither can be recovered from a mono audio pipe. Both would have become
decoration — and a panel where the lamps are decoration is the one thing this
build set out not to be.

## The part that came out better than intended

On a marginal station the STEREO lamp does not sit still. It catches, drops,
catches again as the pilot ratio drifts across the lock threshold — and a real
tuner does exactly this, for exactly the same reason. Nobody wrote that
behaviour. It falls out of measuring the pilot instead of asserting it.

This is the argument for doing the signal processing rather than faking it,
compressed into one lamp: **faked indicators can only do what you thought to
make them do.** Measured ones do what the physics does, including the parts you
did not anticipate.

The same is true of the meter. Tune between stations and it drops to the noise
floor rather than to zero, because that is what the channel actually contains.

## What had to be measured

Both of these were found with `--radio-check`, which sweeps the band and prints
signal, raw dBFS and stereo lock per channel. Neither was predictable from
first principles.

### The AGC had to go

The first sweep read **92% on all 42 channels**. Automatic gain control
normalises the IQ magnitude — which is precisely the quantity the meter reads.
The meter was faithfully reporting the gain loop and telling you nothing about
the station.

`rtlsdr_mt::Controller::enable_agc` also disables manual tuner gain, so the fix
is a fixed gain and no AGC. That costs headroom and buys a meter that means
something.

### The gain had to come *down*

The obvious next move was a high fixed gain — more gain, more signal. Wrong.
At ~30 dB the two **strongest** stations in the band were the only ones failing
to report stereo:

```
    89.0    100%      -6.8       -     <- strongest, no stereo lock
    98.0    100%      -7.4       -     <- second strongest, no stereo lock
    93.5    100%     -14.6     yes
   100.5     86%     -23.7     yes
```

A strong local transmitter saturates the front end, and the resulting
distortion spreads energy into the pilot guard band, collapsing the ratio the
lock test depends on. Dropping to **16.6 dB** fixed it — 89.0 locks stereo now.

### Channel power, not band power

Signal strength is measured *after* the ±100 kHz decimating channel filter.
Measuring the raw 1.024 MHz passband reads the whole FM band at once, which in
a busy market pegs the meter on every frequency.

### Calibration

With the above settled, the band was swept to find the real range rather than
guessing at one:

| | |
|---|---|
| noise floor | ≈ −43 dBFS |
| strongest local | ≈ −19 dBFS |
| meter window | −44 … −18 dBFS |

Different antenna, different market, different numbers. Re-run `--radio-check`
and adjust `FLOOR_DBFS` / `CEIL_DBFS` in `audio/radio.rs`.

## Pilot detection

A threshold on absolute 19 kHz energy does not work: noise has energy there
too, and on a weak signal the discriminator output is large and broadband. The
test is a **ratio** instead —

```
pilot_pow / guard_pow > 6
```

— where the guard band sits at 16.6 kHz, in the gap a correctly-modulated
signal leaves between the mono audio (≤15 kHz) and the pilot. A tone
concentrates; noise spreads. The ratio separates them where an absolute
threshold cannot.

## Seek

SEEK steps the tuner across the band and stops at the first channel whose
signal clears a threshold. **LOCAL** raises that threshold, which is exactly
what the button did in the car: on a motorway you want the scan to skip
everything but the strong stations.

## Threads

Three, because `read_async` blocks:

| | |
|---|---|
| `ten-qd/radio` | opens the device, owns the `Controller`, handles tune and seek |
| `ten-qd/radio-rx` | owns the `Reader`, demodulates inside the read callback |
| `ten-qd/decode` | drains the radio's ring into the output ring |

The radio produces into its own ring rather than the output ring directly. That
keeps a single producer on the output ring, which is what makes the flush
handshake in `audio/mod.rs` sound — two producers could not be paused
coherently.

When the tuner is not the selected source the demodulator stops, but the
channel filter keeps running for its power reading, so SEEK and the meter stay
live without producing audio nobody is listening to.

## Setup

The DVB-T kernel driver claims the dongle on sight:

```sh
sudo pacman -S rtl-sdr                       # ships /usr/lib/modprobe.d/rtlsdr.conf
sudo modprobe -r dvb_usb_rtl28xxu dvb_usb_v2 rtl2832
rtl_test -t                                  # should name the tuner chip
```

The package's udev rules grant device access without a group change or a
re-login.

## No AM

The R820T front end starts around 24 MHz, so the AM broadcast band is out of
reach without a direct-sampling modification. The AM/FM key reports that rather
than switching to a band it cannot receive.
