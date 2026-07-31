//! End-to-end check: a real binary, doing real Opus work, run on the machine that built
//! the archive it links.
//!
//! This is not a duplicate of the test suite. The tests prove the API is wired up
//! correctly; a *binary* proves the three things a test harness cannot:
//!
//! - the static archive links into a shippable executable, in release mode, with no
//!   libopus anywhere on the system and nothing to install (`check-static.sh` asserts
//!   the linkage is static rather than assuming it);
//! - the CPU floor is real — this runs on the runner's own processor, so an archive built
//!   for AVX2 that cannot execute is a failed pipeline rather than a support ticket;
//! - Windows works at all, which is the one target that cannot be built or run anywhere
//!   else in this repository.
//!
//! So it exercises the codec the way a project does: several sample rates and channel
//! counts, which is what moves libopus between its SILK, CELT and hybrid paths — and
//! those paths are where the hand-written SIMD kernels live.
//!
//! Every check reports, and the exit code is the verdict. Failures do not stop the run,
//! because in CI the whole report is more useful than the first line of it.

use opus::{Application, Bitrate, Channels, Decoder, Encoder};
use std::process::ExitCode;

const FRAME_MS: usize = 20;

struct Report {
    passed: usize,
    failed: Vec<String>,
}

impl Report {
    /// One named claim, one line of output. `ok` is the claim being true.
    fn check(&mut self, what: &str, ok: bool) {
        if ok {
            self.passed += 1;
            println!("  ok    {what}");
        } else {
            self.failed.push(what.to_string());
            println!("  FAIL  {what}");
        }
    }
}

/// A 20 ms sine at `rate`, interleaved — the shape libopus wants.
fn tone(rate: u32, channels: usize) -> Vec<i16> {
    let samples = rate as usize / 1000 * FRAME_MS;
    (0..samples * channels)
        .map(|i| {
            let t = (i / channels) as f64 / rate as f64;
            ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16
        })
        .collect()
}

fn rms(pcm: &[i16]) -> f64 {
    (pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt()
}

/// Encode, inspect, decode, compare — for one configuration.
fn round_trip(report: &mut Report, rate: u32, channels: Channels, mode: Application) {
    let n = if channels == Channels::Stereo { 2 } else { 1 };
    let label = format!("{} Hz {}", rate, if n == 2 { "stereo" } else { "mono" });
    println!("\n{label} ({mode:?})");

    let mut encoder = match Encoder::new(rate, channels, mode) {
        Ok(e) => e,
        Err(e) => return report.check(&format!("{label}: create encoder ({e})"), false),
    };
    // Real settings, read back: a CTL that silently addressed the wrong parameter is the
    // failure mode the generated constants could otherwise hide.
    let mut configured = encoder.set_bitrate(Bitrate::Bits(64_000)).is_ok();
    configured &= encoder.set_complexity(8).is_ok();
    configured &= encoder.set_vbr(true).is_ok();
    configured &= encoder.set_inband_fec(true).is_ok();
    configured &= encoder.set_packet_loss_perc(10).is_ok();
    report.check(&format!("{label}: encoder configures"), configured);
    report.check(
        &format!("{label}: bitrate reads back"),
        encoder.get_bitrate().ok() == Some(Bitrate::Bits(64_000)),
    );
    report.check(
        &format!("{label}: complexity reads back"),
        encoder.get_complexity().ok() == Some(8),
    );
    report.check(
        &format!("{label}: sample rate reads back"),
        encoder.get_sample_rate().ok() == Some(rate),
    );

    let input = tone(rate, n);
    let frame = input.len() / n;
    let mut decoder = match Decoder::new(rate, channels) {
        Ok(d) => d,
        Err(e) => return report.check(&format!("{label}: create decoder ({e})"), false),
    };

    // Several frames: the first carry encoder lookahead and decoder warm-up, so only the
    // steady state is worth comparing against the input.
    let mut decoded = vec![0i16; frame * n];
    let mut sizes = Vec::new();
    let mut samples_ok = true;
    for _ in 0..10 {
        let packet = match encoder.encode_vec(&input, 4000) {
            Ok(p) => p,
            Err(e) => return report.check(&format!("{label}: encode ({e})"), false),
        };
        sizes.push(packet.len());

        // The packet-inspection functions, which read the TOC byte rather than decoding —
        // a separate part of the library, and one a project uses for framing.
        samples_ok &= opus::packet::get_nb_samples(&packet, rate).ok() == Some(frame);
        samples_ok &= opus::packet::get_nb_channels(&packet).ok() == Some(channels);
        samples_ok &= opus::packet::get_nb_frames(&packet).ok() == Some(1);
        samples_ok &= decoder.get_nb_samples(&packet).ok() == Some(frame);

        match decoder.decode(&packet, &mut decoded, false) {
            Ok(got) => samples_ok &= got == frame,
            Err(e) => return report.check(&format!("{label}: decode ({e})"), false),
        }
    }

    report.check(&format!("{label}: encodes to packets"), sizes.iter().all(|&s| s > 0));
    report.check(&format!("{label}: packet metadata agrees"), samples_ok);
    report.check(
        &format!("{label}: bandwidth is reported"),
        encoder.get_bandwidth().is_ok(),
    );

    // The one check that a broken codec fails and a merely mislabelled one passes: the
    // decoded signal has to resemble what went in.
    let ratio = rms(&decoded) / rms(&input);
    println!("       {} packets, {}..{} bytes, output/input RMS {ratio:.3}",
             sizes.len(), sizes.iter().min().unwrap(), sizes.iter().max().unwrap());
    report.check(&format!("{label}: signal survives the round trip"), ratio > 0.5 && ratio < 2.0);
}

/// A fixed input, built without a single floating-point operation.
///
/// `tone()` above is unsuitable for a fingerprint, and finding that out cost a wrong claim
/// in a release: it calls `f64::sin`, which on Linux is glibc's, which uses **IFUNC
/// dispatch** to pick an FMA or AVX implementation from the CPU it finds itself on. GitHub's
/// runner fleet spans CPU generations, so the same binary linked against a byte-identical
/// archive produced two different digests on two runs — the *input* differed in its last
/// bits, not the encoder. A fingerprint whose value depends on which machine drew it is
/// worse than none, because it reads as a change in the library.
///
/// Integers only, therefore, and identical on every machine that can run this: a triangle
/// wave for tonal content, plus an xorshift sequence for broadband content, because a
/// silent or pure-tone input exercises far too little of the encoder to be worth hashing.
fn fixed_input(samples: usize) -> Vec<i16> {
    let mut x: u32 = 0x1234_5678;
    (0..samples)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let noise = (x >> 21) as i32 - 1024;
            // 120 samples per cycle at 48 kHz — 400 Hz.
            let phase = (i % 120) as i32;
            let triangle = (if phase < 60 { phase - 30 } else { 90 - phase }) * 220;
            (triangle + noise).clamp(i16::MIN as i32, i16::MAX as i32) as i16
        })
        .collect()
}

/// A fingerprint of what the encoder *produced*, for comparing one build against another.
///
/// The reason this exists: raising the x86_64 floor to AVX2 lets the compiler contract
/// multiply-adds, and libopus's SIMD kernels are not promised to be bit-identical to its
/// scalar ones. So a project with checked-in Opus fixtures has a real question — will its
/// fixtures still match? — and "probably" is not an answer anybody can act on. Printing a
/// digest lets the pipeline compare the Coffee Lake and baseline archives directly and
/// state the result as a fact.
///
/// Every knob that could make this vary for an uninteresting reason is pinned: CBR, fixed
/// complexity, no FEC or DTX, a fixed frame count, and an input built by `fixed_input`
/// rather than by `sin`. What remains is the codec's own arithmetic — and whatever libm
/// *libopus itself* calls, which is the open question the pipeline now answers by running
/// this twice.
fn digest() {
    let mut encoder = Encoder::new(48_000, Channels::Mono, Application::Audio).unwrap();
    encoder.set_bitrate(Bitrate::Bits(64_000)).unwrap();
    encoder.set_vbr(false).unwrap();
    encoder.set_complexity(10).unwrap();
    encoder.set_inband_fec(false).unwrap();
    encoder.set_dtx(false).unwrap();

    let input = fixed_input(48_000 / 1000 * FRAME_MS);
    // FNV-1a over every byte of every packet. Not a cryptographic claim — the only
    // question asked of it is whether two builds produced the same bytes.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut total = 0usize;
    for _ in 0..50 {
        let packet = encoder.encode_vec(&input, 4000).unwrap();
        total += packet.len();
        for byte in &packet {
            hash = (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    println!("libopus {}", opus::version());
    println!("arch    {}", std::env::consts::ARCH);
    println!("bytes   {total}");
    println!("digest  {hash:016x}");
}

fn main() -> ExitCode {
    if std::env::args().any(|a| a == "--digest") {
        digest();
        return ExitCode::SUCCESS;
    }

    let mut report = Report { passed: 0, failed: Vec::new() };

    let version = opus::version();
    println!("libopus:  {version}");
    println!("target:   {}", std::env::consts::ARCH);
    println!("os:       {}", std::env::consts::OS);
    println!("\nlinkage");
    // Which library actually got linked. On a machine with a system libopus installed,
    // this is what catches pkg-config or a stray `-L` winning over the prebuilt archive.
    report.check("libopus is the pinned 1.6 series", version.starts_with("libopus 1.6"));

    // 48 kHz is what both consuming projects use; 16 kHz and 8 kHz push libopus onto its
    // SILK paths, and stereo onto its coupled-channel ones. Different kernels, one binary.
    round_trip(&mut report, 48_000, Channels::Stereo, Application::Audio);
    round_trip(&mut report, 48_000, Channels::Mono, Application::Voip);
    round_trip(&mut report, 16_000, Channels::Mono, Application::Voip);
    round_trip(&mut report, 8_000, Channels::Mono, Application::Voip);

    println!("\nrepacketizer");
    // A different corner of the library again — used for reframing without re-encoding.
    let mut encoder = Encoder::new(48_000, Channels::Mono, Application::Audio).unwrap();
    let input = tone(48_000, 1);
    let a = encoder.encode_vec(&input, 4000).unwrap();
    let b = encoder.encode_vec(&input, 4000).unwrap();
    let mut combined = vec![0u8; a.len() + b.len() + 16];
    match opus::Repacketizer::new().and_then(|mut rp| {
        rp.combine(&[&a, &b], &mut combined).map(|len| (len, ()))
    }) {
        Ok((len, ())) => {
            report.check("two packets combine into one", len > 0);
            report.check(
                "the combined packet holds two frames",
                opus::packet::get_nb_frames(&combined[..len]).ok() == Some(2),
            );
        }
        Err(e) => report.check(&format!("repacketizer ({e})"), false),
    }

    println!();
    if report.failed.is_empty() {
        println!("{} checks passed", report.passed);
        ExitCode::SUCCESS
    } else {
        println!("{} passed, {} FAILED:", report.passed, report.failed.len());
        for what in &report.failed {
            println!("  - {what}");
        }
        ExitCode::FAILURE
    }
}
