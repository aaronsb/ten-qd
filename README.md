# ten-qd

A terminal music player wearing an 80s car stereo.

![The whole rack: seven units, a disc in the tray, the tuner locked on 89.1](docs/img/overview.png)

It plays FLAC, MP3, AAC, Vorbis and WAV; it compiles folders into two-sided
tapes; and it receives broadcast FM off an RTL-SDR. The equaliser is eighteen
real biquads, the meters are a real FFT of the real output, and the STEREO lamp
lights off a real 19 kHz pilot.

**It is not a faithful reproduction of a Fujitsu Ten stack**, and stopped
trying to be fairly early on. The advertisement that started it is the design
language — the colours, the glyph vocabulary, the way a lit segment reads
against an unlit one — not a specification ([see the ad](docs/the-ad.md)). Where authenticity and a good
instrument disagreed, the instrument won: the amp meters run vertically so they
line up with the equaliser bands, and the QD-585 CD player is a unit Fujitsu
Ten never built.

What survived from the original design brief is the part worth keeping: **every
lit thing on the panel is connected to something real.** No decorative meters,
no indicator that is always on.

## Everything modern turns out to have an ancestor

The thing that kept happening while building this: every feature a music
player is supposed to have in 2026 already existed in 1985, under a different
name, solving the same problem. The 80s vocabulary is not a costume over
modern concepts — it is a set of concepts that were already correct.

| now | then |
|---|---|
| a folder of audio files | a disc, and its file order is the table of contents |
| an M3U playlist | a compiled tape, split into two sides by running time |
| Spotify, YouTube Music, Apple Music | the **AUX** input — a wire from the thing in your pocket |
| a PipeWire null sink | the wire itself |
| MPRIS over D-Bus | reaching over to hit next on the Discman |
| a settings file in `~/.local/state` | the **12-volt memory**, on the battery feed |
| a terminal's character grid | a vacuum-fluorescent display, already |
| a dark-mode toggle | the dash rheostat on the instrument lighting |
| a system-wide EQ like EasyEffects | the QE-581, second bay from the bottom |

![A disc playing, the equaliser curved, the amplifier's meters moving](docs/img/rack-cd.png)

The last two rows are the ones that changed the build. The dimmer became a
luminance scale on the token layer because a dash dimmer is *exactly* that.
And AUX is EasyEffects' virtual-sink trick with a better name for the
cable — which means the rack is a general audio processor: anything that can
choose an output device gets the equaliser, the tone controls, the fader and
the meters. Discord will offer it as an output. That is not a side effect, it
is the design.

## Why a terminal

The first pass was an HTML page, and it worked. But shipping that as an
application means Electron or Tauri — a browser engine, a build pipeline and a
few hundred megabytes of runtime, to draw some lit rectangles.

A character grid is the period-correct medium. Something built between the mid
80s and the mid 90s would have had a fixed grid of cells, each either lit or
not, and a small palette — which is *exactly* what a vacuum-fluorescent display
is. So the constraint turns out to be generative rather than limiting:
seven-segment digits assembled from quadrant blocks read more like a real VFD
than a photorealistic render does, because they are made the same way. The
[ghost segments](docs/design.md#the-ghost) are only convincing because the
medium genuinely cannot do anything smoother.

The practical version of the same argument: this is one 5.9 MB binary that
starts instantly, uses no GPU, and works over SSH.

## Building it

Three system libraries beyond Rust itself — ALSA (cpal's Linux backend),
librtlsdr (the tuner), and pkg-config to find them. `make deps` checks all of
it and prints the install line for your package manager if anything is missing:

```sh
make deps        # check build and runtime dependencies
make run         # build optimised and start the panel
make install     # copy to ~/.local/bin (XDG user scope)
make help        # every target
```

`make install` puts the binary in `$XDG_DATA_HOME/bin`, defaulting to
`~/.local/bin`, and tells you if that is not on your `PATH`. `make uninstall`
removes it and leaves your settings alone; `make forget` clears those too.
Override the location with `make install PREFIX=/usr/local`.

Rust 1.85 or newer (edition 2024). `make check` runs clippy with warnings as
errors, plus the tests.

## Running it

```sh
make run                                # first album found under ~/Music
cargo run --release -- /path/to/album   # a folder of audio files is a disc
cargo run --release -- mix.m3u          # a playlist is a tape, in its own order
make screenshot                         # render one frame to stdout and exit
make radio-check                        # sweep the FM band and report signal
```

`M` picks what REC records. In **TRACK** mode the deck logs everything any
player on the desktop plays — what it was, and for how long — and writes no
audio at all. A playlist is something you cut out of that afterwards:

```sh
ten-qd --log                     # what the log holds, by session
ten-qd --export=last > mix.m3u   # the session that just ended
ten-qd --export=2026-07 --rank   # a month, ordered by play count
```

The point is a week of YouTube Music in a tab and Spotify in the afternoon
coming out as one list you hold, in a format any player can open.

In **AUDIO** mode `R` arms the deck — meters live, nothing written — so the
record level can be set against real signal, and again to roll. After that `R`
pauses and resumes, and `s` ends the take: a pause leaves the file open and the
COUNTER standing, so one take is one file however many times you stop for. It
writes WAV tapped *before* the equaliser, so the volume can be anywhere and the
curve you set for your headphones is not baked into the file. See
[docs/recording.md](docs/recording.md).

Wants a terminal at least **84 columns** wide and 24-bit colour, with a font
that has good block-element coverage — any Nerd Font will do. The rack is 87
rows tall and scrolls with PgUp/PgDn or the wheel.

**Every control is clickable.** Sliders take a click anywhere along their
travel, the volume bar takes a click at the level you want, and the music
calendar cues a track. Press `?` in the app for the full key map.

| | |
|---|---|
| `c` `t` `u` | source: compact disc · cassette · tuner |
| `o` | open the disc/tape browser |
| `SPACE` `s` | play/pause · stop (acts on the selected source) |
| `←` `→` / `p` `n` | previous · next  (tuner: seek) |
| `1`–`9` | cue a track  (tuner: recall preset) |
| `!` … `^` | store the current station as a preset (kept between runs) |
| `[` `]` `g` `P` | tune down/up · LOCAL · tuner power |
| `e` `r` `z` | eject · repeat · random |
| `v` `y` `b` | flip the tape · Dolby · auto-reverse |
| `M` `R` | record mode: TRACK a list · AUDIO the signal — then arm, roll, pause |
| `s` | AUDIO: ends the take — one file across every pause |
| `( )` | record level, ±12 dB — its own stage, upstream of volume and GAIN |
| `a`  `A` | select AUX · pick what to send through it |
| `1`–`9` | …on AUX: plug that stream through the rack |
| `↑` `↓` `m` | volume · attenuator |
| `,` `.` / `<` `>` | bass · treble |
| `;` `'` | fader rear/front |
| `h` `l` / `j` `k` | select equaliser band · cut/boost |
| `f` `d` `0` | front/rear bank · defeat · flat |
| `{` `}` | equaliser output trim, ±12 dB — cut the curve back, or make up a quiet source |
| `i` `w` | illumination colour · amplifier power |
| `-` `=` | instrument dimmer, down and up |
| `O` | pick the output device the rack drives |
| `~` | arrange the rack — reorder units, take them out, put them back |
| click `POWER` | take a unit out of the signal path, or put it back |
| `C` `T` `U` `X` `E` `W` `H` | fold a unit away: CD, cassette, tuner, aux, EQ, amp, control head |

`?` shows the same thing without leaving the panel:

![The key map, over the rack](docs/img/rack-keys.png)

### The radio

Needs an RTL-SDR, and needs the DVB-T kernel driver out of the way — it claims
the device on sight. `make deps` will tell you if it has:

```sh
sudo modprobe -r dvb_usb_rtl28xxu dvb_usb_v2 rtl2832
rtl_test -t                                  # should name the tuner chip
```

Without one the panel says so and the other three sources are unaffected. See
[docs/tuner.md](docs/tuner.md) for how the demodulator works and what had to be
measured to make the meters honest.

## Documentation

| | |
|---|---|
| [docs/sources.md](docs/sources.md) | the four sources — what a disc, a tape, a station and a cable each are |
| [docs/audio.md](docs/audio.md) | the signal path, the DSP, and the clock the display runs on |
| [docs/recording.md](docs/recording.md) | REC and the listening log — a mixtape is an order, not a copy |
| [docs/tuner.md](docs/tuner.md) | FM demodulation on an RTL-SDR, and calibrating it against the band |
| [docs/design.md](docs/design.md) | the panel language: glyphs, colour, and what is invented |
| [docs/memory.md](docs/memory.md) | the 12-volt memory: what persists, where, and how it heals |
| [docs/the-ad.md](docs/the-ad.md) | the 1986 advertisement this came from, and what was taken from it |
| [docs/panel-reference.html](docs/panel-reference.html) | the original HTML prototype this was ported from |

## Layout

| | |
|---|---|
| `ui/theme.rs` | tokens. Every colour in the build, and nothing else. |
| `ui/glyph.rs` | the character vocabulary — seven-segment, meters, furniture. |
| `ui/chassis.rs` | bays, windows, lamps, keys. Knows nothing about CD players. |
| `ui/units/*` | one module per component. No unit reaches into another. |
| `ui/hit.rs` | click targets, registered by the units as they draw. |
| `ui/overlay.rs` | the key map and the browser panel. |
| `audio/mod.rs` | decode thread, output callback, the clock. |
| `audio/dsp.rs` | biquads and the parameter snapshot. |
| `audio/analysis.rs` | the 9-band analyser, on its own thread. |
| `audio/radio.rs` | the FM demodulator and the SDR threads. |
| `disc.rs` | a folder is a disc; its file order is the TOC. |
| `browser.rs` | the shelf — what to put in the machine, and how. |
| `state.rs` | unit state, and the one function allowed to mutate it. |
| `memory.rs` | the 12-volt memory — what survives the ignition going off. |

## Known limits

- **No AM.** The R820T front end starts around 24 MHz, so the AM broadcast band
  is out of reach without a direct-sampling modification. The band key says so
  rather than tuning nothing.
- **Front and rear sum to stereo.** A four-channel device would let the two
  equaliser banks drive four speakers; here the fader crossfades between the
  two curves. Audible and honest about itself, but not the same thing.
- **Resampling is 4-point Hermite**, and it is in the path constantly (48 kHz
  device, 44.1 kHz discs). `rubato` is the drop-in upgrade; `hermite()` in
  `audio/mod.rs` is the only function that would change.
