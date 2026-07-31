//! The tests this fork adds, on top of the three files inherited from upstream `opus`.
//!
//! They exist because of where the risk actually is. The safe wrapper is upstream's,
//! unchanged, and upstream's own tests cover it. What is new here is underneath: an FFI
//! layer written out by hand and a table of ~90 integer constants generated from the
//! opus headers. A wrong constant is not a compile error — it is a CTL that quietly
//! configures a *different* parameter than the one named, or returns `BadArg` in
//! production. So every setting the projects consuming this crate touch gets set and
//! read back here, which is the only way a wrong number shows up as a failing test
//! rather than as bad audio months later.

extern crate opus;

use opus::{Application, Bitrate, Channels, Decoder, Encoder};

const RATE: u32 = 48_000;
const FRAME: usize = 960; // 20 ms at 48 kHz, which is what both consumers use

/// A quiet sine, in the shape libopus wants: interleaved i16, one 20 ms frame.
fn frame(channels: usize) -> Vec<i16> {
    (0..FRAME * channels)
        .map(|i| {
            let t = (i / channels) as f64 / RATE as f64;
            ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16
        })
        .collect()
}

/// `save_audio_stream`'s configuration, exactly: mono VoIP at a fixed bitrate.
#[test]
fn encodes_like_save_audio_stream() {
    let mut encoder = Encoder::new(RATE, Channels::Mono, Application::Voip).unwrap();
    encoder.set_bitrate(Bitrate::Bits(64_000)).unwrap();

    // Every one of these is a distinct pair of generated constants, and each assertion
    // fails if either half of the pair is wrong.
    assert_eq!(encoder.get_bitrate().unwrap(), Bitrate::Bits(64_000));
    assert_eq!(encoder.get_application().unwrap(), Application::Voip);
    assert_eq!(encoder.get_sample_rate().unwrap(), RATE);

    let packet = encoder.encode_vec(&frame(1), 4000).unwrap();
    assert!(!packet.is_empty(), "encoded nothing");
    assert_eq!(opus::packet::get_nb_channels(&packet).unwrap(), Channels::Mono);
    assert_eq!(opus::packet::get_nb_samples(&packet, RATE).unwrap(), FRAME);
}

/// `remotex`'s side: a stereo decoder fed real packets.
#[test]
fn decodes_like_remotex() {
    let mut encoder = Encoder::new(RATE, Channels::Stereo, Application::Audio).unwrap();
    let packet = encoder.encode_vec(&frame(2), 4000).unwrap();

    let mut decoder = Decoder::new(RATE, Channels::Stereo).unwrap();
    assert_eq!(decoder.get_nb_samples(&packet).unwrap(), FRAME);

    let mut pcm = vec![0i16; FRAME * 2];
    assert_eq!(decoder.decode(&packet, &mut pcm, false).unwrap(), FRAME);
    // A decode that returned the right *length* but silence would mean the packet never
    // made it through, and `decode` reports success either way.
    assert!(pcm.iter().any(|&s| s != 0), "decoded to pure silence");
}

/// The rest of the CTLs, set-then-read. Grouped rather than split into one test per
/// setting because the failure they guard against is identical in each case.
#[test]
fn every_ctl_round_trips() {
    use opus::{Bandwidth, FrameSize, Signal};

    let mut e = Encoder::new(RATE, Channels::Stereo, Application::Audio).unwrap();

    e.set_complexity(7).unwrap();
    assert_eq!(e.get_complexity().unwrap(), 7);
    e.set_vbr(false).unwrap();
    assert!(!e.get_vbr().unwrap());
    e.set_vbr_constraint(true).unwrap();
    assert!(e.get_vbr_constraint().unwrap());
    e.set_bitrate(Bitrate::Bits(96_000)).unwrap();
    assert_eq!(e.get_bitrate().unwrap(), Bitrate::Bits(96_000));
    // `Max` is a sentinel on the way in only: libopus resolves it and reports back the
    // concrete ceiling, so this asserts the resolution rather than a round trip.
    e.set_bitrate(Bitrate::Max).unwrap();
    match e.get_bitrate().unwrap() {
        Bitrate::Bits(bits) => assert!(bits >= 500_000, "Max resolved to only {} bps", bits),
        other => panic!("Max resolved to {:?}", other),
    }
    e.set_signal(Signal::Voice).unwrap();
    assert_eq!(e.get_signal().unwrap(), Signal::Voice);
    e.set_max_bandwidth(Bandwidth::Wideband).unwrap();
    assert_eq!(e.get_max_bandwidth().unwrap(), Bandwidth::Wideband);
    e.set_inband_fec(true).unwrap();
    assert!(e.get_inband_fec().unwrap());
    e.set_packet_loss_perc(15).unwrap();
    assert_eq!(e.get_packet_loss_perc().unwrap(), 15);
    e.set_dtx(true).unwrap();
    assert!(e.get_dtx().unwrap());
    e.set_lsb_depth(16).unwrap();
    assert_eq!(e.get_lsb_depth().unwrap(), 16);
    e.set_expert_frame_duration(FrameSize::Ms20).unwrap();
    assert_eq!(e.get_expert_frame_duration().unwrap(), FrameSize::Ms20);
    e.set_prediction_disabled(true).unwrap();
    assert!(e.get_prediction_disabled().unwrap());
    e.set_force_channels(Some(Channels::Mono)).unwrap();
    assert_eq!(e.get_force_channels().unwrap(), Some(Channels::Mono));
    e.set_phase_inversion_disabled(true).unwrap();
    assert!(e.get_phase_inversion_disabled().unwrap());
    assert!(e.get_lookahead().unwrap() > 0, "no encoder lookahead reported");
    e.reset_state().unwrap();

    let mut d = Decoder::new(RATE, Channels::Stereo).unwrap();
    d.set_gain(-256).unwrap();
    assert_eq!(d.get_gain().unwrap(), -256);
    d.reset_state().unwrap();
}

/// Encode → decode → measure. The one test here that would catch a *broken* library
/// rather than a mislabelled one: a build with the wrong CPU dispatch, or an archive
/// that is not the opus it claims to be, does not reproduce a sine wave.
#[test]
fn round_trip_reproduces_the_signal() {
    let mut encoder = Encoder::new(RATE, Channels::Mono, Application::Audio).unwrap();
    encoder.set_bitrate(Bitrate::Bits(128_000)).unwrap();
    let input = frame(1);

    let mut decoder = Decoder::new(RATE, Channels::Mono).unwrap();
    let mut decoded = vec![0i16; FRAME];
    // Opus needs a few frames before its output is worth comparing — the first is spent
    // on encoder lookahead and decoder warm-up.
    for _ in 0..5 {
        let packet = encoder.encode_vec(&input, 4000).unwrap();
        decoder.decode(&packet, &mut decoded, false).unwrap();
    }

    let energy = |pcm: &[i16]| pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>().sqrt();
    let ratio = energy(&decoded) / energy(&input);
    // Explicit format arguments: this crate is edition 2015, where a lone string is
    // passed through verbatim rather than treated as a format string.
    assert!(
        ratio > 0.5 && ratio < 2.0,
        "decoded energy is {:.3}x the input — the codec path is not working",
        ratio
    );
}

/// Which library got linked. On a machine with a system libopus installed, this is the
/// test that fails if pkg-config or a stray `-L` ever wins over the prebuilt archive.
#[test]
fn links_the_pinned_libopus() {
    let version = opus::version();
    println!("linked: {}", version);
    assert!(version.starts_with("libopus 1.6"), "unexpected libopus: {}", version);
}
