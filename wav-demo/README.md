# wav-demo

A WAV goes in, Opus happens to it, a WAV comes out — so you can listen to the difference.

```sh
cargo run --release                                  # synthesises a 10 s clip first
cargo run --release -- some.wav
cargo run --release -- some.wav --bitrates 16,32,64,128
```

```
libopus:  libopus 1.6.1
input:    48000 Hz, stereo, 10.00 s, 480000 frames

    kbps    packets    bytes    ratio    actual   SNR dB
      24        500    30769    62.4x     24.6k     11.7
      64        500    80500    23.9x     64.4k     16.6
     128        500   160500    12.0x    128.4k     20.1
```

For each bitrate it writes into `out/`:

| file | what it is |
|---|---|
| `<name>-<kbps>k.opus` | **the real thing** — Ogg Opus, playable in any player |
| `<name>-original.wav` | the input, so the A/B is against a file and not a memory |
| `<name>-<kbps>k.wav` | the same stream decoded back, time-aligned with the original |
| `<name>-<kbps>k-diff.wav` | what Opus discarded, gained up so it is audible |

```sh
ffplay -autoexit out/demo-24k.opus     # or VLC, or drag it into a browser
afplay out/demo-original.wav
afplay out/demo-24k.wav
afplay out/demo-24k-diff.wav           # the error signal on its own
```

`afplay` and QuickTime do not read Opus; `ffplay`, VLC, Firefox and Chrome all do. That is
what the decoded `.wav` is for — same audio, no player problem.

The difference file is the interesting one. At 128 kbps it needs +11 dB to be audible at
all and sounds like faint hiss around the transients; at 24 kbps it needs only +3 dB and
you can hear the hi-hats in it, which is the codec telling you where it spent its bits.

## The .opus files are real files, not a demo format

`encode_vec` returns bare Opus packets — no lengths, no timestamps, no channel count. That
is right for a network protocol, where the transport carries all of it, and useless as a
file. `src/ogg.rs` adds the Ogg container from RFC 7845: the `OpusHead` and `OpusTags`
header packets, page framing with segment lacing, granule positions, and Ogg's CRC-32
(polynomial `0x04c11db7`, unreflected — *not* the CRC-32 from zip, which silently produces
a file every player rejects). No dependency; it is about 150 lines.

Verified against tools that had no part in writing it:

```
$ opusinfo out/demo-24k.opus
  Pre-skip: 312          Channels: 2        Original sample rate: 48000 Hz
  Packet duration: 20.0ms (max/avg/min)     Playback length: 0m:10.000s
  Total data length: 31686 bytes (overhead: 2.69%)
```

Zero warnings on all three files, and the playback length is exactly 10.000 s — the last
frame is zero-padded and followed by a flush packet, so a wrong granule position on the
final page would show up here as a fraction of a second of extra silence.

Decoding our file with Xiph's own `opusdec` and comparing to our decode:

| | identical samples | largest difference |
|---|---|---|
| `opusdec` default | 13.7% | — (62.8 dB SNR) |
| `opusdec --no-dither` | **99.98%** | **1 LSB** |

The first row is `opusdec`'s output dithering, not a disagreement about the audio. The
second is the real answer: the same packets, decoded by an independent implementation, to
within float-to-integer rounding.

## The tag is deliberately not kept current

`Cargo.toml` pins `opus-prebuilt` to a git tag, and **that tag does not get bumped when a
new release is cut.** Nothing in CI builds this directory. An old tag still resolves, and
`build.rs` fetches archives from the repository's *latest* release regardless of which tag
the crate source came from — so a stale pin here demonstrates exactly what a current one
would. Bumping it on every release would mean a commit whose only content is a tag, which
is the maintenance the whole `releases/latest/download/…` design exists to avoid.

Bump it when you are demonstrating something new, or if the tag stops existing.

## Notes on what it does

**Time alignment.** Opus has an algorithmic delay — 312 samples at 48 kHz — so the decode
is offset from the input. The encoder reports it via `get_lookahead()` and that many
samples are dropped from the front, which is why `-original.wav` and `-24k.wav` line up
sample-for-sample. Measured on the demo clip, SNR peaks at lag 0 (20.5 dB) and falls to
13.8 dB by ±8 samples, so the compensation is exact rather than approximately right.

**Sample rates.** Opus accepts 8, 12, 16, 24 and 48 kHz only, and there is no resampler
here. A 44.1 kHz file is rejected with the conversion command to run:

```sh
afconvert -f WAVE -d LEI16@48000 in.wav out.wav      # macOS
ffmpeg -i in.wav -ar 48000 -c:a pcm_s16le out.wav
```

**SNR is not quality.** It is reported because it is cheap and it moves in the right
direction, but Opus is not trying to preserve the waveform — at low rates CELT preserves
*band energy* and lets the fine structure go, which sounds fine and scores badly. Noise-like
content can round-trip at 3 dB SNR and be hard to tell apart by ear. Trust your ears over
the column.

**16-bit PCM only**, mono or stereo. The WAV reader walks the RIFF chunk list rather than
assuming a 44-byte header, so files with a `LIST`/`INFO` chunk before `data` work.
