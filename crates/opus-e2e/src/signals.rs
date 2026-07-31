//! Test signals, and the correlation each one is allowed to lose.
//!
//! Four classes rather than one tone, because Opus behaves very differently depending on
//! what it is given, and a single sine would let three of the four failure modes through:
//! SILK and CELT split by bandwidth, coupled stereo differs from mono, and noise-like
//! content goes down a path that does not preserve the waveform at all.
//!
//! That last one is why the floor travels with the signal rather than being one constant.
//! CELT codes noise with PVQ, which preserves the energy in each band and lets the fine
//! structure go — so a perfectly healthy encoder round-trips noise at a correlation that
//! would be alarming for a chord. One global threshold would either pass a broken tonal
//! path or fail a working noise one.

pub struct Signal {
    pub name: &'static str,
    /// Mono, at the rate it was asked for.
    pub samples: Vec<f64>,
    /// The lowest correlation this class may show and still be considered working.
    /// Every value here sits well below what was measured — see the README.
    pub corr_floor: f64,
}

/// A deterministic PRNG, so the noise is the same noise on every target. `f64::sin` below
/// is not bit-identical across platforms — glibc dispatches it per CPU — but these are
/// approximate comparisons and a last-place-bit difference in the input cannot move a band
/// energy by decibels. That distinction is the whole reason this file measures rather than
/// hashes.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

fn sine(rate: u32, n: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..n).map(|i| (i as f64 / rate as f64 * hz * std::f64::consts::TAU).sin() * amp).collect()
}

pub fn all(rate: u32, n: usize) -> Vec<Signal> {
    let nyquist = rate as f64 / 2.0;
    let mut rng = Rng(0x5eed);

    let tone = sine(rate, n, 440.0, 8000.0);

    let mut chord = sine(rate, n, 220.0, 4000.0);
    for (dst, src) in chord.iter_mut().zip(sine(rate, n, 330.0, 3000.0)) {
        *dst += src;
    }
    for (dst, src) in chord.iter_mut().zip(sine(rate, n, 550.0, 2000.0)) {
        *dst += src;
    }

    // Band-limited noise. A one-pole low-pass keeps the content under the codec's
    // bandwidth, so the comparison is not dominated by high frequencies Opus discards on
    // purpose — that would measure the bandwidth limit, not the codec.
    let mut lp = 0.0;
    let noise: Vec<f64> = (0..n)
        .map(|_| {
            lp = 0.85 * lp + 0.15 * rng.next_f64() * 8000.0;
            lp
        })
        .collect();

    // Speech-like: a 120 Hz pulse train through two resonators, amplitude modulated. This
    // is the class the consuming projects actually carry, and the one SILK is tuned for.
    let period = (rate as usize / 120).max(1);
    let (w1, w2) = (700.0f64.min(nyquist * 0.4), 1800.0f64.min(nyquist * 0.8));
    let (mut f1, mut f1v, mut f2, mut f2v) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let voiced: Vec<f64> = (0..n)
        .map(|i| {
            let excite = if i % period == 0 { 6000.0 } else { 0.0 };
            f1v += (excite - f1 * (w1 / rate as f64) - f1v * 0.05) * (w1 / rate as f64);
            f1 += f1v;
            f2v += (excite - f2 * (w2 / rate as f64) - f2v * 0.08) * (w2 / rate as f64);
            f2 += f2v;
            let env = 0.6 + 0.4 * (i as f64 / rate as f64 * 3.0 * std::f64::consts::TAU).sin();
            (f1 * 0.7 + f2 * 0.3) * env
        })
        .collect();

    vec![
        Signal { name: "sine", samples: tone, corr_floor: 0.99 },
        Signal { name: "chord", samples: chord, corr_floor: 0.95 },
        Signal { name: "speech", samples: voiced, corr_floor: 0.95 },
        // The outlier, and deliberately so. Measured as low as 0.65 at 8 kHz / 12 kbps on
        // a working encoder; anything near a global 0.95 would fail every clean build.
        Signal { name: "noise", samples: noise, corr_floor: 0.60 },
    ]
}
