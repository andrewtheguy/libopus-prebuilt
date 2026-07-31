//! Just enough WAV to read and write 16-bit PCM, with no dependency.
//!
//! Reading walks the RIFF chunk list rather than assuming a 44-byte header, because plenty
//! of real files carry a LIST/INFO chunk before `data` and skipping straight to byte 44
//! silently reads metadata as audio.

pub struct Wav {
    pub rate: u32,
    pub channels: u16,
    /// Interleaved.
    pub samples: Vec<i16>,
}

impl Wav {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.rate as f64
    }
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

pub fn read(path: &std::path::Path) -> Result<Wav, String> {
    let b = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()));
    }

    let (mut fmt, mut data) = (None, None);
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32le(&b[pos + 4..pos + 8]) as usize;
        let body = pos + 8;
        let end = (body + size).min(b.len());
        match id {
            b"fmt " => fmt = Some(&b[body..end]),
            b"data" => data = Some(&b[body..end]),
            _ => {}
        }
        // Chunks are word-aligned: an odd size is followed by a pad byte.
        pos = body + size + (size & 1);
    }

    let fmt = fmt.ok_or("no fmt chunk")?;
    let data = data.ok_or("no data chunk")?;
    if fmt.len() < 16 {
        return Err("fmt chunk is too short".into());
    }

    let format = u16le(&fmt[0..2]);
    let channels = u16le(&fmt[2..4]);
    let rate = u32le(&fmt[4..8]);
    let bits = u16le(&fmt[14..16]);

    // 1 = PCM, 0xFFFE = WAVE_FORMAT_EXTENSIBLE, which is still PCM when bits is 16 and the
    // sub-format GUID says so. Accepting it makes files from most recorders work.
    if format != 1 && format != 0xFFFE {
        return Err(format!("format {format} is not PCM — convert with `afconvert` or `ffmpeg` first"));
    }
    if bits != 16 {
        return Err(format!("{bits}-bit samples; this demo handles 16-bit PCM only"));
    }
    if channels != 1 && channels != 2 {
        return Err(format!("{channels} channels; Opus here handles mono or stereo"));
    }

    let samples = data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    Ok(Wav { rate, channels, samples })
}

pub fn write(path: &std::path::Path, wav: &Wav) -> Result<(), String> {
    let bytes_per_frame = wav.channels as u32 * 2;
    let data_len = (wav.samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&wav.channels.to_le_bytes());
    out.extend_from_slice(&wav.rate.to_le_bytes());
    out.extend_from_slice(&(wav.rate * bytes_per_frame).to_le_bytes()); // byte rate
    out.extend_from_slice(&(bytes_per_frame as u16).to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &wav.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }

    std::fs::write(path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}
