//! Ways to compare two signals that are *supposed* to differ.
//!
//! Opus is lossy, so nothing here can be an equality test — and bit-exact comparison is
//! doubly wrong for this repository, because the four archives take different SIMD paths
//! and float code that reorders an addition does not produce the same bits. The digests
//! this project used to publish failed for exactly that family of reasons.
//!
//! So: three measures, and the reason there are three is that each is blind to something.
//! Correlation ignores gain but is fooled by a constant offset in time. SNR is intuitive
//! and collapses on noise-like content, because CELT preserves band energy there rather
//! than the waveform. Band energy is phase-blind and survives both, which is why it is the
//! one the thresholds lean on.

/// Pearson correlation. Insensitive to overall gain, unlike SNR.
pub fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let ma = a[..n].iter().sum::<f64>() / n as f64;
    let mb = b[..n].iter().sum::<f64>() / n as f64;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        return 0.0;
    }
    num / (da.sqrt() * db.sqrt())
}

/// The delay, in samples, that best lines the decode up with the reference.
///
/// `get_lookahead()` is the honest answer — the noise check in main.rs finds it exact, off
/// by zero, at every rate tested — but it is not usable as a *fixed* offset here, for two
/// separate reasons the measurements separate cleanly:
///
///   - **Periodic content is ambiguous.** A 440 Hz tone at 16 kHz repeats every 36.4
///     samples, and the search returns 212 against a reported 104: three periods late,
///     correlating 0.9999 either way. Harmless, and unavoidable.
///   - **Some content genuinely shifts.** The speech-like signal at 16 kHz aligns at 88,
///     not 104, and this one is not a period — its pitch period is 133 samples. Forcing
///     104 drops correlation to 0.7180, which is what a 16-sample offset does to a signal
///     with that period: cos(16/133 x 360 degrees) = 0.73.
///
/// The second is why an earlier version of this file failed six checks against a perfectly
/// healthy encoder. Searching costs a slice of one correlation per lag and removes the
/// whole question.
///
/// Searched on a slice rather than the whole signal: the answer is the same and this runs
/// under valgrind, where the difference is tens of seconds.
pub fn best_delay(reference: &[f64], decoded: &[f64], hint: usize, span: usize) -> usize {
    let hi = hint + span;
    // The window has to leave room for the *largest* delay tried, or the loop breaks on
    // the first lag that runs off the end and the answer is whatever it happened to try
    // first. At 8 kHz a one second signal is 8000 samples, so a fixed 8192 window left
    // room for exactly one candidate — delay 0 — and every metric downstream then compared
    // signals that were 52 samples apart and reported a broken codec.
    let window = 8192.min(reference.len()).min(decoded.len().saturating_sub(hi));
    if window < 256 {
        return hint.min(decoded.len());
    }

    let mut best = (hint, f64::NEG_INFINITY);
    for delay in hint.saturating_sub(span)..=hi {
        let c = correlation(&reference[..window], &decoded[delay..delay + window]);
        if c > best.1 {
            best = (delay, c);
        }
    }
    best.0
}

/// Signal-to-noise ratio in dB, sample-aligned. Reported rather than asserted on for
/// noise-like content — see the module note.
pub fn snr_db(reference: &[f64], decoded: &[f64]) -> f64 {
    let n = reference.len().min(decoded.len());
    let mut signal = 0.0;
    let mut noise = 0.0;
    for i in 0..n {
        signal += reference[i] * reference[i];
        noise += (reference[i] - decoded[i]).powi(2);
    }
    if noise == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (signal / noise).log10()
    }
}

const BAND_EDGES: [f64; 9] =
    [0.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0, 24000.0];

/// The largest per-band energy difference, in dB, over the bands that carry content.
///
/// "Carry content" means within 40 dB of the loudest band. Without that gate the maximum
/// is decided by bands where the reference has essentially nothing and the codec is
/// entitled to put nothing at all — an early version of this measured 12.9 dB on a signal
/// whose worst *real* band was off by 0.3 dB.
pub fn max_band_error_db(reference: &[f64], decoded: &[f64], rate: u32) -> f64 {
    let a = band_energies(reference, rate);
    let b = band_energies(decoded, rate);
    let peak = a.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    a.iter()
        .zip(&b)
        .filter(|(r, _)| **r > peak - 40.0)
        .map(|(r, o)| (r - o).abs())
        .fold(0.0f64, f64::max)
}

fn band_energies(x: &[f64], rate: u32) -> Vec<f64> {
    // A power of two, because the FFT below is radix-2, and no more than 4096 because the
    // bands are wide and more resolution buys nothing.
    let n = 4096.min((x.len() + 1).next_power_of_two() / 2).max(2);
    let spec = magnitudes(&x[..n.min(x.len())]);
    let bin_hz = rate as f64 / n as f64;
    BAND_EDGES
        .windows(2)
        .map(|w| {
            let lo = ((w[0] / bin_hz) as usize).min(spec.len());
            let hi = ((w[1] / bin_hz) as usize).min(spec.len());
            let e: f64 = spec[lo..hi].iter().map(|m| m * m).sum();
            // The floor keeps log10 finite for a band the signal never reaches, and sits
            // far below anything audible at 16-bit scale.
            10.0 * (e + 1e-12).log10()
        })
        .collect()
}

/// Radix-2 FFT, magnitudes only. Length must be a power of two.
fn magnitudes(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    debug_assert!(n.is_power_of_two());
    let mut re = x.to_vec();
    let mut im = vec![0.0; n];

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let ang = -std::f64::consts::TAU / len as f64;
        for i in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((ang * k as f64).cos(), (ang * k as f64).sin());
                let (ur, ui) = (re[i + k], im[i + k]);
                let half = i + k + len / 2;
                let (vr, vi) = (re[half] * wr - im[half] * wi, re[half] * wi + im[half] * wr);
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[half] = ur - vr;
                im[half] = ui - vi;
            }
        }
        len <<= 1;
    }

    (0..n / 2).map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt()).collect()
}
