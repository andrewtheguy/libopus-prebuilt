//! Wrap Opus packets in an Ogg container, so the result is a `.opus` file a player will
//! open — RFC 7845.
//!
//! `Encoder::encode_vec` hands back bare Opus packets with no framing at all: no lengths,
//! no timestamps, no channel count. That is the right thing for a network protocol, where
//! the transport already carries all of it, and useless as a file. Ogg supplies the
//! missing parts — packet boundaries, a sample clock, and the two header packets that say
//! how many channels there are and how much of the start to throw away.
//!
//! Deliberately no dependency, to keep the demo's Cargo.toml one line long.

/// Everything the container needs that the packets themselves do not carry.
pub struct Stream {
    pub channels: u8,
    /// Encoder delay, **in 48 kHz samples** — the field is defined at 48 kHz whatever the
    /// input rate is. A player discards this many samples so the file starts where the
    /// original did.
    pub pre_skip: u16,
    /// The original rate. Informational only: Opus always decodes at whatever rate is
    /// asked for, and this field exists so a player can say where the audio came from.
    pub input_rate: u32,
    /// 48 kHz samples per packet, for the timestamps.
    pub samples_per_packet: u64,
    /// Where the audio really ends, in 48 kHz samples including `pre_skip`. The last frame
    /// is zero-padded and there is a flush packet after it, so without this a player would
    /// render up to a frame of silence that was never in the input.
    pub final_granule: u64,
}

pub fn write(path: &std::path::Path, packets: &[Vec<u8>], s: &Stream) -> Result<(), String> {
    let mut out = Vec::new();
    // Fixed rather than random: this demo has one stream and reruns should differ only
    // where the audio differs.
    let serial = 0x4F505553;
    let mut seq = 0u32;

    // Page 1, beginning-of-stream, carrying OpusHead alone as the spec requires.
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(s.channels);
    head.extend_from_slice(&s.pre_skip.to_le_bytes());
    head.extend_from_slice(&s.input_rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain, Q7.8 dB
    head.push(0); // channel mapping family 0: mono or stereo, no mapping table
    page(&mut out, 0x02, 0, serial, &mut seq, &[&head]);

    // Page 2, OpusTags. The vendor string is conventionally the encoder's own version.
    let vendor = opus::version();
    let comment = format!("ENCODER=wav-demo ({})", env!("CARGO_PKG_NAME"));
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor.as_bytes());
    tags.extend_from_slice(&1u32.to_le_bytes()); // one user comment
    tags.extend_from_slice(&(comment.len() as u32).to_le_bytes());
    tags.extend_from_slice(comment.as_bytes());
    page(&mut out, 0x00, 0, serial, &mut seq, &[&tags]);

    // Audio pages. Several packets per page, because a page header is 27 bytes plus the
    // segment table and a 20 ms packet at 24 kbps is only about 60 bytes — one page each
    // would spend nearly half the file on framing.
    let mut batch: Vec<&[u8]> = Vec::new();
    let mut segments = 0usize;
    let mut bytes = 0usize;
    let mut done = 0u64;

    for (i, packet) in packets.iter().enumerate() {
        let need = packet.len() / 255 + 1;
        // 255 segments is the hard limit per page; 4 KB is a soft one, so that a page loss
        // costs a sensible amount of audio.
        if !batch.is_empty() && (segments + need > 255 || bytes + packet.len() > 4096) {
            page(&mut out, 0x00, done * s.samples_per_packet, serial, &mut seq, &batch);
            batch.clear();
            segments = 0;
            bytes = 0;
        }
        batch.push(packet);
        segments += need;
        bytes += packet.len();
        done = i as u64 + 1;

        if i + 1 == packets.len() {
            // End of stream, and the granule position that trims the padding.
            page(&mut out, 0x04, s.final_granule, serial, &mut seq, &batch);
        }
    }

    std::fs::write(path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// One Ogg page: the fixed header, the segment table, then the packet bodies.
fn page(out: &mut Vec<u8>, header_type: u8, granule: u64, serial: u32, seq: &mut u32, packets: &[&[u8]]) {
    let start = out.len();

    // Segment table. A packet ends at the first segment shorter than 255, which is why a
    // packet whose length is an exact multiple of 255 needs a trailing zero.
    let mut table = Vec::new();
    for p in packets {
        let mut remaining = p.len();
        while remaining >= 255 {
            table.push(255u8);
            remaining -= 255;
        }
        table.push(remaining as u8);
    }
    assert!(table.len() <= 255, "too many segments for one page");

    out.extend_from_slice(b"OggS");
    out.push(0); // stream structure version
    out.push(header_type);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    let crc_at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes()); // CRC, filled in below
    out.push(table.len() as u8);
    out.extend_from_slice(&table);
    for p in packets {
        out.extend_from_slice(p);
    }

    // The checksum covers the whole page with its own field zeroed, which is why it is
    // written last rather than computed as we go.
    let crc = crc32(&out[start..]);
    out[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
    *seq += 1;
}

/// Ogg's CRC-32: polynomial 0x04c11db7, initial value zero, **no** input or output
/// reflection and no final xor. That is not the CRC-32 in zip or PNG, and using the
/// familiar one produces a file every player rejects.
fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04c1_1db7 } else { r << 1 };
            }
            *slot = r;
        }
        t
    });

    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ table[(((crc >> 24) as u8) ^ b) as usize];
    }
    crc
}
