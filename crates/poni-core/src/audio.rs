//! The microphone's samples and the WAV files made of them, shared by the
//! Notes recorder: 16 kHz mono 16-bit PCM, the way the transcriber wants it.
//! Pure: no device, no file system beyond the writer's own file.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The recording's format: 16 kHz mono 16-bit PCM in a WAV file.
pub const RATE: u32 = 16_000;
/// A WAV file takes no more than this long (the Notes recorder starts a new part before).
pub const RECORD_MAX_S: f32 = 250.0;

// ---------------------------------------------------------------- WAV

/// A canonical 44-byte WAV header for PCM.
pub fn wav_header(rate: u32, channels: u16, bits: u16, data_len: u32) -> [u8; 44] {
    let block = channels * bits / 8;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block as u32).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// A WAV file's format and length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WavInfo {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub secs: f32,
}

/// Read a WAV file's `fmt ` and `data` chunks (PCM only), walking the RIFF
/// chunks: recorders put others before the audio (the Mac's 4 KB `FLLR`
/// padding, for one), so the header isn't always 44 bytes.
pub fn wav_info(bytes: &[u8]) -> Option<WavInfo> {
    riff(bytes).map(|(info, _)| info)
}

/// The same audio behind the canonical 44-byte header, every other chunk
/// left out (what the answer uploads). None for a file that isn't PCM WAV.
pub fn canonical_wav(bytes: &[u8]) -> Option<Vec<u8>> {
    let (info, data) = riff(bytes)?;
    let audio = &bytes[data];
    let mut out = Vec::with_capacity(44 + audio.len());
    out.extend(wav_header(info.rate, info.channels, info.bits, audio.len() as u32));
    out.extend_from_slice(audio);
    Some(out)
}

/// The format and where the audio is.
fn riff(bytes: &[u8]) -> Option<(WavInfo, std::ops::Range<usize>)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let (mut fmt, mut data) = (None, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        // A recorder that never filled in its sizes: the rest of the file.
        let end = (at + 8).saturating_add(len).min(bytes.len());
        let body = &bytes[at + 8..end];
        match id {
            b"fmt " if body.len() >= 16 => {
                let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
                // WAVE_FORMAT_EXTENSIBLE: the old tag is the sub-format GUID's first field.
                let tag = if u16_at(0) == 0xFFFE && body.len() >= 26 { u16_at(24) } else { u16_at(0) };
                fmt = Some((tag, u16_at(2), u32::from_le_bytes([body[4], body[5], body[6], body[7]]), u16_at(14)));
            }
            b"data" => data = Some(at + 8..end),
            _ => {}
        }
        at = end + (len & 1);
    }
    let (tag, channels, rate, bits) = fmt?;
    let data = data?;
    if tag != 1 || channels == 0 || rate == 0 || bits < 8 {
        return None;
    }
    let secs = data.len() as f32 / (rate as f32 * channels as f32 * (bits / 8) as f32);
    Some((WavInfo { rate, channels, bits, secs }, data))
}

/// Writes the answer as it is recorded: 16 kHz mono 16-bit, the header's
/// sizes filled in at the end.
pub struct WavWriter {
    out: BufWriter<File>,
    path: PathBuf,
    samples: u64,
    buf: Vec<u8>,
}

impl WavWriter {
    pub fn create(path: &Path) -> io::Result<WavWriter> {
        let mut out = BufWriter::with_capacity(64 * 1024, File::create(path)?);
        out.write_all(&wav_header(RATE, 1, 16, 0))?;
        Ok(WavWriter { out, path: path.to_path_buf(), samples: 0, buf: Vec::new() })
    }

    /// Append samples, up to [`RECORD_MAX_S`] in all (the rest is dropped).
    pub fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        let room = (RECORD_MAX_S as u64 * RATE as u64).saturating_sub(self.samples) as usize;
        let pcm = &pcm[..pcm.len().min(room)];
        if pcm.is_empty() {
            return Ok(());
        }
        self.buf.clear();
        self.buf.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
        self.out.write_all(&self.buf)?;
        self.samples += pcm.len() as u64;
        Ok(())
    }

    pub fn secs(&self) -> f32 {
        self.samples as f32 / RATE as f32
    }

    /// Fill in the sizes and close the file: its path and length in seconds.
    pub fn finish(self) -> io::Result<(PathBuf, f32)> {
        let secs = self.secs();
        let data = (self.samples * 2) as u32;
        let mut f = self.out.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&wav_header(RATE, 1, 16, data))?;
        f.flush()?;
        Ok((self.path, secs))
    }
}

/// A WAV of `secs` of a soft `hz` tone at `rate` (16-bit mono): the
/// self-test's stand-in answer and voice, so nothing needs a microphone.
pub fn tone_wav(secs: f32, hz: f32, rate: u32) -> Vec<u8> {
    let n = (secs * rate as f32) as usize;
    let mut out = wav_header(rate, 1, 16, (n * 2) as u32).to_vec();
    for i in 0..n {
        let v = (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin() * 0.25;
        out.extend(((v * 32767.0) as i16).to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------- the microphone's samples

/// How the microphone's samples are stored (Windows' mix format, usually 32-bit float).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    I16,
    /// 24-bit in 3 bytes.
    I24,
    I32,
}

impl SampleFormat {
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::I16 => 2,
            SampleFormat::I24 => 3,
            SampleFormat::F32 | SampleFormat::I32 => 4,
        }
    }

    fn read(self, b: &[u8]) -> f32 {
        match self {
            SampleFormat::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            SampleFormat::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            SampleFormat::I24 => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            SampleFormat::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        }
    }
}

/// A second-order low-pass (RBJ's cookbook), transposed direct form II.
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn low_pass(rate: f32, cutoff: f32, q: f32) -> Biquad {
        let w = std::f32::consts::TAU * cutoff / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b1 = (1.0 - cos) / a0;
        Biquad { b0: b1 / 2.0, b1, b2: b1 / 2.0, a1: -2.0 * cos / a0, a2: (1.0 - alpha) / a0, z1: 0.0, z2: 0.0 }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Turns the microphone's frames (any rate, channels and sample format)
/// into 16 kHz mono 16-bit: the channels averaged, a 4th-order low-pass
/// under 8 kHz (so nothing above it folds back into the speech), then
/// linear interpolation to 16 kHz. It keeps its place across packets, so
/// packets of any size give the same samples.
#[derive(Clone, Debug)]
pub struct Downmix {
    format: SampleFormat,
    channels: usize,
    /// Source samples per output sample.
    step: f64,
    /// Where the next output sample sits, in source samples after `prev`.
    pos: f64,
    prev: f32,
    filters: Option<[Biquad; 2]>,
}

impl Downmix {
    pub fn new(rate: u32, channels: u16, format: SampleFormat) -> Downmix {
        let rate = rate.max(1);
        // Butterworth, 4th order: two stages with these Qs, cut at 0.45 × 16 kHz.
        let filters = (rate > RATE + 1000).then(|| [0.541_196, 1.306_563].map(|q| Biquad::low_pass(rate as f32, 0.45 * RATE as f32, q)));
        Downmix { format, channels: channels.max(1) as usize, step: rate as f64 / RATE as f64, pos: 1.0, prev: 0.0, filters }
    }

    /// Interleaved frames in (`bytes`), 16 kHz samples onto `out`. Returns
    /// the loudest of them, 0..1 (the level meter).
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<i16>) -> f32 {
        let (size, channels) = (self.format.bytes(), self.channels);
        let start = out.len();
        for frame in bytes.chunks_exact(size * channels) {
            let sum: f32 = frame.chunks_exact(size).map(|s| self.format.read(s)).sum();
            self.feed(sum / channels as f32, out);
        }
        peak(&out[start..])
    }

    /// `frames` frames of silence (a packet the system flagged silent).
    pub fn silence(&mut self, frames: usize, out: &mut Vec<i16>) {
        for _ in 0..frames {
            self.feed(0.0, out);
        }
    }

    fn feed(&mut self, x: f32, out: &mut Vec<i16>) {
        let x = match self.filters.as_mut() {
            Some([a, b]) => b.run(a.run(x)),
            None => x,
        };
        while self.pos <= 1.0 {
            let y = self.prev + (x - self.prev) * self.pos as f32;
            out.push((y.clamp(-1.0, 1.0) * 32767.0).round() as i16);
            self.pos += self.step;
        }
        self.pos -= 1.0;
        self.prev = x;
    }
}

/// The loudest sample, 0..1.
pub fn peak(pcm: &[i16]) -> f32 {
    pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_writer_fills_in_the_header() {
        let dir = std::env::temp_dir().join(format!("poni-ielts-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let mut w = WavWriter::create(&path).unwrap();
        w.write(&vec![1000i16; 8000]).unwrap();
        w.write(&vec![-1000i16; 8000]).unwrap();
        assert_eq!(w.secs(), 1.0);
        let (p, secs) = w.finish().unwrap();
        assert_eq!((p.as_path(), secs), (path.as_path(), 1.0));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 32_000);
        assert_eq!(wav_info(&bytes), Some(WavInfo { rate: 16_000, channels: 1, bits: 16, secs: 1.0 }));
        assert_eq!(wav_info(&tone_wav(0.5, 440.0, 22_050)).map(|i| (i.rate, i.secs)), Some((22_050, 0.5)));
        assert_eq!(wav_info(b"RIFF\0\0\0\0WAVE"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mac_recording_s_padding_chunk_is_walked_past_and_left_out() {
        // Apple's recorder writes `fmt `, then a 4 KB `FLLR` chunk, then the audio.
        let audio: Vec<u8> = (0..32_000u32).map(|i| (i % 251) as u8).collect();
        let canonical = wav_header(16_000, 1, 16, audio.len() as u32);
        let mut mac = b"RIFF\0\0\0\0WAVE".to_vec();
        mac.extend_from_slice(&canonical[12..36]);
        mac.extend(b"FLLR");
        mac.extend(4044u32.to_le_bytes());
        mac.extend(vec![0u8; 4044]);
        mac.extend(b"data");
        mac.extend((audio.len() as u32).to_le_bytes());
        mac.extend(&audio);
        assert_eq!(mac.len() - audio.len(), 4096, "a 4096-byte header");
        assert_eq!(wav_info(&mac), Some(WavInfo { rate: 16_000, channels: 1, bits: 16, secs: 1.0 }));
        let clean = canonical_wav(&mac).unwrap();
        assert_eq!(&clean[..44], &canonical);
        assert_eq!(&clean[44..], &audio[..]);
        // A data size never filled in reads to the end of the file.
        let mut open = mac.clone();
        let at = open.len() - audio.len() - 4;
        open[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(wav_info(&open).map(|i| i.secs), Some(1.0));
        // Not PCM WAV: nothing to clean.
        assert_eq!(canonical_wav(b"ID3\x04 not a wav"), None);
    }

    fn sine(rate: u32, hz: f32, secs: f32, channels: usize) -> Vec<u8> {
        let n = (rate as f32 * secs) as usize;
        let mut out = Vec::new();
        for i in 0..n {
            let v = (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin() * 0.5;
            for _ in 0..channels {
                out.extend(v.to_le_bytes());
            }
        }
        out
    }

    /// Crossings of zero going up, per second.
    fn freq(pcm: &[i16]) -> f32 {
        let ups = pcm.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        ups as f32 / (pcm.len() as f32 / RATE as f32)
    }

    #[test]
    fn downmix_resamples_speech_to_16k_mono() {
        // 48 kHz stereo float (the usual Windows mix format), a 1 kHz tone.
        let mut d = Downmix::new(48_000, 2, SampleFormat::F32);
        let mut out = Vec::new();
        let p = d.push(&sine(48_000, 1000.0, 1.0, 2), &mut out);
        assert!((out.len() as i64 - 16_000).abs() <= 1, "{}", out.len());
        assert!((freq(&out) - 1000.0).abs() < 5.0, "{}", freq(&out));
        assert!((0.45..0.55).contains(&p), "peak {p}");
        // 44.1 kHz 16-bit mono: the length follows the rate.
        let pcm: Vec<u8> = sine(44_100, 500.0, 1.0, 1).chunks(4).flat_map(|b| (((f32::from_le_bytes(b.try_into().unwrap())) * 32767.0) as i16).to_le_bytes()).collect();
        let mut d = Downmix::new(44_100, 1, SampleFormat::I16);
        let mut out = Vec::new();
        d.push(&pcm, &mut out);
        assert!((out.len() as i64 - 16_000).abs() <= 1, "{}", out.len());
        assert!((freq(&out) - 500.0).abs() < 5.0);
    }

    #[test]
    fn downmix_keeps_tones_above_8k_from_folding_back() {
        let mut d = Downmix::new(48_000, 1, SampleFormat::F32);
        let mut out = Vec::new();
        d.push(&sine(48_000, 12_000.0, 1.0, 1), &mut out);
        // Unfiltered, 12 kHz would land at 4 kHz at full strength (0.5).
        assert!(peak(&out[800..]) < 0.05, "{}", peak(&out[800..]));
    }

    #[test]
    fn downmix_gives_the_same_samples_whatever_the_packets() {
        let input = sine(44_100, 700.0, 0.5, 2);
        let mut whole = Vec::new();
        Downmix::new(44_100, 2, SampleFormat::F32).push(&input, &mut whole);
        let mut parts = Vec::new();
        let mut d = Downmix::new(44_100, 2, SampleFormat::F32);
        for chunk in input.chunks(8 * 37) {
            d.push(chunk, &mut parts);
        }
        assert_eq!(whole, parts);
        // 16 kHz mono 16-bit comes through as it is.
        let pcm: Vec<i16> = (0..1000).map(|i| (i * 13 % 2000 - 1000) as i16).collect();
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        Downmix::new(16_000, 1, SampleFormat::I16).push(&bytes, &mut out);
        let back: Vec<i16> = out.iter().map(|s| ((*s as f32 / 32767.0) * 32768.0).round() as i16).collect();
        assert!(back.iter().zip(&pcm).all(|(a, b)| (a - b).abs() <= 1));
        // 24-bit samples read right.
        let mut out = Vec::new();
        Downmix::new(16_000, 1, SampleFormat::I24).push(&[0x00, 0x00, 0x40, 0x00, 0x00, 0xC0], &mut out);
        assert_eq!(out, [16384, -16384]);
        let mut out = Vec::new();
        Downmix::new(48_000, 2, SampleFormat::F32).silence(4800, &mut out);
        assert_eq!((out.len(), peak(&out)), (1600, 0.0));
    }
}
