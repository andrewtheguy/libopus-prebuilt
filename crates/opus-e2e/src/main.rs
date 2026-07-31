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

// ---------- memory ----------
//
// If upstream libopus does not leak, the only way this project can is a `*_create` with no
// matching destroy — five allocating entry points in the FFI, five `Drop` impls. What
// follows checks that pairing three ways, because each tool covers a platform the others
// cannot: this in-process churn test runs everywhere including Windows, valgrind runs under
// `test-docker.sh`, and `leaks` runs on the macOS CI job.
//
// A leak detector that cannot see libopus's allocations reports zero and looks like a pass.
// libopus calls C `malloc` directly, so a Rust `GlobalAlloc` counter would be exactly that
// kind of blind instrument — it observes only Rust-side allocation and would report zero
// however badly the handles leaked. So every mode here is paired with a control that leaks
// on purpose, and the control **failing to be detected** is itself a failed check.

/// Resident set size in KB. Three implementations because there is no portable one, and
/// none of them needs a dependency.
fn rss_kb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // VmRSS is already in kB, which sidesteps the page size — 4 K on most arm64 Linux
        // but not guaranteed, and a hardcoded 4096 would silently misreport by 16x.
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Process -Id {}).WorkingSet64", std::process::id()),
            ])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok().map(|b| b / 1024)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

/// Create every kind of handle `n` times. With `leak`, the handles are forgotten instead
/// of dropped — which is the control, and the only place in this repository that does it.
fn churn(n: usize, leak: bool) {
    let pcm = vec![0i16; 1920];
    let mut out = vec![0i16; 1920];
    for _ in 0..n {
        let mut enc = Encoder::new(48_000, Channels::Stereo, Application::Audio).unwrap();
        let mut dec = Decoder::new(48_000, Channels::Stereo).unwrap();
        let rp = opus::Repacketizer::new().unwrap();
        // Encode and decode as well as allocate: a handle that is created and immediately
        // destroyed may never touch the buffers a real one does.
        let packet = enc.encode_vec(&pcm, 4000).unwrap();
        dec.decode(&packet, &mut out, false).unwrap();
        if leak {
            std::mem::forget(enc);
            std::mem::forget(dec);
            std::mem::forget(rp);
        }
    }
}

/// Churn with a settled baseline, reporting how far RSS moved.
fn rss_growth(n: usize, leak: bool) -> Option<i64> {
    // Warm up first. The allocator grows its arenas on the first few handles, and counting
    // that as a leak would make the honest path fail.
    churn(64, false);
    let before = rss_kb()?;
    churn(n, leak);
    let after = rss_kb()?;
    Some(after as i64 - before as i64)
}

fn memory_checks(report: &mut Report) {
    println!("\nmemory");
    // Under valgrind this section is both redundant and painfully slow — twelve thousand
    // encoder states, each memset by the codec, on an instrumented allocator. valgrind is
    // measuring the same property exactly, so `test-docker.sh` turns this off rather than
    // waiting for it.
    if std::env::var("OPUS_E2E_MEMORY").as_deref() == Ok("off") {
        println!("  skip  OPUS_E2E_MEMORY=off — an exact leak checker is doing this instead");
        return;
    }
    let Some(_) = rss_kb() else {
        println!("  skip  no RSS on this platform — valgrind and leaks still cover it");
        return;
    };

    // The control runs first and on purpose: 3 handles x 400 is roughly 10 MB of libopus
    // state, which the measurement must be able to see. If it cannot, every other result
    // in this section is worthless and this check says so.
    let leaked = rss_growth(400, true).unwrap_or(0);
    println!("  ..    leaking 1200 handles moved RSS by {leaked} KB");
    report.check(
        "the measurement can see a deliberate leak",
        leaked > 4_000,
    );

    // And the real thing. Ten times the churn of the control, all of it dropped.
    let honest = rss_growth(4_000, false).unwrap_or(i64::MAX);
    println!("  ..    12000 handles created and dropped moved RSS by {honest} KB");
    report.check("creating and dropping handles does not grow RSS", honest < 4_000);
}

/// Leak on purpose and exit. Not reachable by accident — it exists so that valgrind and
/// `leaks` can be pointed at a process that definitely leaks, proving those tools see
/// libopus's allocations before their clean runs are believed.
///
/// Far fewer handles than the RSS control needs: those tools count bytes exactly, so three
/// would do, where RSS has to clear the noise of a moving heap. Keeping it small matters
/// because this runs under valgrind, where every allocation is instrumented.
fn leak_only() -> ExitCode {
    println!("--leak-only: forgetting 60 libopus handles on purpose");
    println!("a leak detector that reports nothing here cannot see libopus at all");
    churn(20, true);
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    if std::env::args().any(|a| a == "--leak-only") {
        return leak_only();
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

    memory_checks(&mut report);

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
