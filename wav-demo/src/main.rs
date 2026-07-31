//! Encode a WAV through Opus and decode it back, so you can listen to the result.
//!
//!   cargo run --release                     # synthesises a demo clip first
//!   cargo run --release -- some.wav
//!   cargo run --release -- some.wav --bitrates 16,32,64,128
//!
//! For each bitrate it writes `out/<name>-<kbps>k.wav` (the round trip) and
//! `out/<name>-<kbps>k-diff.wav` (what Opus discarded, gained up so it is audible), plus
//! `out/<name>-original.wav` to A/B against.
//!
//! The decoded file is time-aligned with the original: Opus adds an algorithmic delay, and
//! the encoder reports it via `get_lookahead()`, so that many samples are dropped from the
//! front of the decode. Without it the two files are offset by a few milliseconds, which
//! makes an A/B comparison sound like a phase problem that isn't there.

mod ogg;
mod wav;

use opus::{Application, Bitrate, Channels, Decoder, Encoder};
use std::path::{Path, PathBuf};
use wav::Wav;

const FRAME_MS: usize = 20;
/// The rates libopus accepts. Anything else needs resampling, which this demo does not do.
const RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input: Option<PathBuf> = None;
    let mut bitrates = vec![24, 64, 128];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bitrates" => {
                i += 1;
                let list = args.get(i).ok_or("--bitrates needs a value, e.g. 32,64")?;
                bitrates = list
                    .split(',')
                    .map(|s| s.trim().parse::<i32>().map_err(|_| format!("not a number: {s}")))
                    .collect::<Result<_, _>>()?;
            }
            "-h" | "--help" => {
                println!("usage: wav-demo [input.wav] [--bitrates 24,64,128]");
                return Ok(());
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other => input = Some(PathBuf::from(other)),
        }
        i += 1;
    }

    let out_dir = Path::new("out");
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create out/: {e}"))?;

    let (source, stem) = match &input {
        Some(p) => {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            (wav::read(p)?, stem)
        }
        None => {
            println!("no input given — synthesising a demo clip\n");
            (demo_clip(), "demo".to_string())
        }
    };

    if !RATES.contains(&source.rate) {
        return Err(format!(
            "{} Hz is not an Opus sample rate ({}).\n       Convert first, e.g.\n         \
             afconvert -f WAVE -d LEI16@48000 in.wav out.wav      (macOS)\n         \
             ffmpeg -i in.wav -ar 48000 -c:a pcm_s16le out.wav",
            source.rate,
            RATES.map(|r| r.to_string()).join(", "),
        ));
    }

    println!("libopus:  {}", opus::version());
    println!(
        "input:    {} Hz, {}, {:.2} s, {} frames",
        source.rate,
        if source.channels == 2 { "stereo" } else { "mono" },
        source.seconds(),
        source.frames(),
    );

    let original = out_dir.join(format!("{stem}-original.wav"));
    wav::write(&original, &source)?;
    println!("wrote     {}", original.display());
    println!();

    let raw_bits = source.samples.len() as f64 * 16.0;
    println!(
        "  {:>6}  {:>9}  {:>7}  {:>7}  {:>8}  {:>7}",
        "kbps", "packets", "bytes", "ratio", "actual", "SNR dB"
    );

    let lowest = bitrates.iter().copied().min().unwrap_or(64);
    for kbps in &bitrates {
        let kbps = *kbps;
        let trip = round_trip(&source, kbps)?;
        let total_bytes = trip.bytes();

        let snr = snr_db(&source.samples, &trip.decoded.samples);
        println!(
            "  {:>6}  {:>9}  {:>7}  {:>6.1}x  {:>7.1}k  {:>7.1}",
            kbps,
            trip.packets.len(),
            total_bytes,
            raw_bits / (total_bytes as f64 * 8.0),
            total_bytes as f64 * 8.0 / source.seconds() / 1000.0,
            snr,
        );

        // The real thing: the compressed stream in an Ogg container, playable anywhere.
        // Everything Ogg needs beyond the packets is derived here — the timestamps are in
        // 48 kHz units regardless of the input rate, which is the one part of RFC 7845
        // that is easy to get wrong at 8 or 24 kHz and produces a file that plays at the
        // wrong speed.
        let to_48k = |n: u64| n * 48_000 / source.rate as u64;
        let opus_path = out_dir.join(format!("{stem}-{kbps}k.opus"));
        ogg::write(
            &opus_path,
            &trip.packets,
            &ogg::Stream {
                channels: source.channels as u8,
                pre_skip: to_48k(trip.lookahead as u64) as u16,
                input_rate: source.rate,
                samples_per_packet: to_48k(source.rate as u64 / 1000 * FRAME_MS as u64),
                final_granule: to_48k(source.frames() as u64) + to_48k(trip.lookahead as u64),
            },
        )?;

        let wav_path = out_dir.join(format!("{stem}-{kbps}k.wav"));
        wav::write(&wav_path, &trip.decoded)?;

        // What the codec threw away. At unity it is usually far too quiet to hear, so it
        // is normalised to -3 dBFS and the gain is reported — the number is as interesting
        // as the sound, since it says how far below the signal the error sits.
        let (diff, gain_db) = difference(&source, &trip.decoded);
        let diff_path = out_dir.join(format!("{stem}-{kbps}k-diff.wav"));
        wav::write(&diff_path, &diff)?;

        println!(
            "          -> {}  ({} KB, playable)\n          -> {}  and  {} (+{:.0} dB to be audible)",
            opus_path.display(),
            total_bytes / 1024,
            wav_path.display(),
            diff_path.display(),
            gain_db,
        );
    }

    // The lowest bitrate, because that is the one where there is something to hear.
    println!(
        "\nListen:\n  \
         afplay out/{stem}-original.wav          the input\n  \
         ffplay -autoexit out/{stem}-{lowest}k.opus    the .opus file itself\n  \
         afplay out/{stem}-{lowest}k.wav              the same thing decoded to WAV\n  \
         afplay out/{stem}-{lowest}k-diff.wav         only what the codec discarded"
    );
    Ok(())
}

struct Trip {
    decoded: Wav,
    /// The compressed stream, one entry per 20 ms frame — what goes into the `.opus` file.
    packets: Vec<Vec<u8>>,
    /// Encoder delay at the input rate.
    lookahead: usize,
}

impl Trip {
    fn bytes(&self) -> usize {
        self.packets.iter().map(|p| p.len()).sum()
    }
}

/// Encode every frame and decode it straight back, returning PCM aligned with the input.
fn round_trip(source: &Wav, kbps: i32) -> Result<Trip, String> {
    let ch = if source.channels == 2 { Channels::Stereo } else { Channels::Mono };
    let nch = source.channels as usize;
    let frame = source.rate as usize / 1000 * FRAME_MS;

    let mut enc = Encoder::new(source.rate, ch, Application::Audio)
        .map_err(|e| format!("cannot create the encoder: {e}"))?;
    enc.set_bitrate(Bitrate::Bits(kbps * 1000)).map_err(|e| e.to_string())?;
    enc.set_complexity(10).map_err(|e| e.to_string())?;
    let lookahead = enc.get_lookahead().map_err(|e| e.to_string())? as usize;

    let mut dec = Decoder::new(source.rate, ch).map_err(|e| e.to_string())?;

    // The tail is zero-padded to a whole frame, and the extra samples are trimmed off the
    // decode below — Opus has no notion of a partial frame.
    let mut input = source.samples.clone();
    while input.len() % (frame * nch) != 0 {
        input.push(0);
    }

    let mut decoded = Vec::with_capacity(input.len() + lookahead * nch);
    let mut buf = vec![0i16; frame * nch];
    let mut packets = Vec::with_capacity(input.len() / (frame * nch) + 1);

    for chunk in input.chunks_exact(frame * nch) {
        let packet = enc.encode_vec(chunk, 4000).map_err(|e| format!("encode failed: {e}"))?;
        let got = dec.decode(&packet, &mut buf, false).map_err(|e| format!("decode failed: {e}"))?;
        decoded.extend_from_slice(&buf[..got * nch]);
        packets.push(packet);
    }

    // One more empty frame, so the tail still inside the decoder comes out. It goes into
    // the .opus file too — the granule position on the last page is what tells a player to
    // stop before the padding, rather than the packet being absent.
    let packet = enc.encode_vec(&vec![0i16; frame * nch], 4000).map_err(|e| e.to_string())?;
    if let Ok(got) = dec.decode(&packet, &mut buf, false) {
        decoded.extend_from_slice(&buf[..got * nch]);
    }
    packets.push(packet);

    // Drop the codec delay from the front, then match the original length exactly.
    let skip = (lookahead * nch).min(decoded.len());
    let mut samples = decoded[skip..].to_vec();
    samples.resize(source.samples.len(), 0);

    Ok(Trip {
        decoded: Wav { rate: source.rate, channels: source.channels, samples },
        packets,
        lookahead,
    })
}

/// original - decoded, normalised so it can actually be heard. Returns the gain applied.
fn difference(a: &Wav, b: &Wav) -> (Wav, f64) {
    let raw: Vec<f64> =
        a.samples.iter().zip(&b.samples).map(|(&x, &y)| x as f64 - y as f64).collect();
    let peak = raw.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1.0);
    // -3 dBFS.
    let target = 32767.0 * 0.708;
    let gain = target / peak;
    let samples = raw.iter().map(|v| (v * gain).clamp(-32768.0, 32767.0) as i16).collect();
    (Wav { rate: a.rate, channels: a.channels, samples }, 20.0 * gain.log10())
}

fn snr_db(reference: &[i16], decoded: &[i16]) -> f64 {
    let n = reference.len().min(decoded.len());
    let mut signal = 0.0;
    let mut noise = 0.0;
    for i in 0..n {
        let (r, d) = (reference[i] as f64, decoded[i] as f64);
        signal += r * r;
        noise += (r - d) * (r - d);
    }
    if noise == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (signal / noise).log10()
    }
}

/// A short stereo clip with something to listen to: a four-chord progression with plucked
/// envelopes, a bass line, and a hi-hat, so the round trip has transients, tonal content
/// and noise in it rather than a single sine.
fn demo_clip() -> Wav {
    let rate = 48_000u32;
    let bpm = 96.0;
    let beat = 60.0 / bpm;
    let bars = 4;
    let total = (beat * 4.0 * bars as f64 * rate as f64) as usize;

    // Am - F - C - G, as semitone offsets from A2 (110 Hz).
    let chords: [[f64; 3]; 4] = [
        [0.0, 3.0, 7.0],    // Am
        [-4.0, 0.0, 5.0],   // F
        [3.0, 7.0, 12.0],   // C
        [-2.0, 2.0, 7.0],   // G
    ];
    let hz = |semi: f64| 110.0 * 2.0f64.powf(semi / 12.0);

    let mut left = vec![0.0f64; total];
    let mut right = vec![0.0f64; total];
    let mut rng = Rng(0xC0FFEE);

    for bar in 0..bars {
        let chord = chords[bar % chords.len()];
        let bar_start = (beat * 4.0 * bar as f64 * rate as f64) as usize;

        // Chord: one pluck per beat, slightly detuned across the stereo field.
        for b in 0..4 {
            let start = bar_start + (beat * b as f64 * rate as f64) as usize;
            for (v, &semi) in chord.iter().enumerate() {
                let f = hz(semi + 12.0);
                let pan = v as f64 / (chord.len() - 1) as f64; // 0 = left, 1 = right
                for i in 0..(beat * 1.6 * rate as f64) as usize {
                    let t = i as f64 / rate as f64;
                    let env = (-t * 4.5).exp();
                    // Two partials, so it is not a bare sine.
                    let s = ((t * f * std::f64::consts::TAU).sin() * 0.7
                        + (t * f * 2.0 * std::f64::consts::TAU).sin() * 0.3)
                        * env
                        * 3200.0;
                    let n = start + i;
                    if n < total {
                        left[n] += s * (1.0 - pan * 0.7);
                        right[n] += s * (0.3 + pan * 0.7);
                    }
                }
            }
        }

        // Bass: root on beats 1 and 3.
        for b in [0usize, 2] {
            let start = bar_start + (beat * b as f64 * rate as f64) as usize;
            let f = hz(chord[0]);
            for i in 0..(beat * 1.4 * rate as f64) as usize {
                let t = i as f64 / rate as f64;
                let env = (-t * 3.0).exp();
                let s = (t * f * std::f64::consts::TAU).sin() * env * 5000.0;
                let n = start + i;
                if n < total {
                    left[n] += s;
                    right[n] += s;
                }
            }
        }

        // Hi-hat on every eighth: filtered noise, which is the content a codec has the
        // hardest time with and the easiest place to hear artefacts.
        for e in 0..8 {
            let start = bar_start + (beat * 0.5 * e as f64 * rate as f64) as usize;
            let mut hp = 0.0;
            let mut prev = 0.0;
            for i in 0..(0.06 * rate as f64) as usize {
                let t = i as f64 / rate as f64;
                let white = rng.next_f64();
                hp = 0.7 * (hp + white - prev);
                prev = white;
                let s = hp * (-t * 60.0).exp() * 2600.0;
                let n = start + i;
                if n < total {
                    left[n] += s * 0.9;
                    right[n] += s * 1.1;
                }
            }
        }
    }

    // Interleave with a little headroom.
    let peak = left.iter().chain(right.iter()).fold(0.0f64, |m, v| m.max(v.abs())).max(1.0);
    let g = 32767.0 * 0.85 / peak;
    let mut samples = Vec::with_capacity(total * 2);
    for i in 0..total {
        samples.push((left[i] * g).clamp(-32768.0, 32767.0) as i16);
        samples.push((right[i] * g).clamp(-32768.0, 32767.0) as i16);
    }
    Wav { rate, channels: 2, samples }
}

/// Deterministic, so the demo clip is the same clip every time and on every machine.
struct Rng(u64);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}
