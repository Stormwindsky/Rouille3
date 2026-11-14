//! Sound playback (.wav / .ogg / .mp3 / .flac ...) through rodio.
//!
//! Decoding strategy, so that "weird" files from Scratch projects still play:
//!   1. rodio / symphonia (wav, ogg vorbis, mp3, flac, aac, adpcm ...)
//!   2. a tolerant built-in WAV reader (bad chunk sizes, 8/16/24/32-bit, float, IMA ADPCM)
//!   3. the external `ffmpeg` program as a last resort (separate process)
#![allow(dead_code)]
use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// A sound ready to be played many times without decoding it again.
enum Prepared {
    Raw(Arc<Vec<u8>>),
    Pcm { channels: u16, rate: u32, data: Arc<Vec<i16>> },
    Fail,
}

enum Snd {
    Dec(Decoder<Cursor<Vec<u8>>>),
    Buf(SamplesBuffer<i16>),
}

impl Iterator for Snd {
    type Item = i16;
    fn next(&mut self) -> Option<i16> {
        match self {
            Snd::Dec(d) => d.next(),
            Snd::Buf(b) => b.next(),
        }
    }
}

impl Source for Snd {
    fn current_frame_len(&self) -> Option<usize> {
        match self { Snd::Dec(d) => d.current_frame_len(), Snd::Buf(b) => b.current_frame_len() }
    }
    fn channels(&self) -> u16 {
        match self { Snd::Dec(d) => d.channels(), Snd::Buf(b) => b.channels() }
    }
    fn sample_rate(&self) -> u32 {
        match self { Snd::Dec(d) => d.sample_rate(), Snd::Buf(b) => b.sample_rate() }
    }
    fn total_duration(&self) -> Option<Duration> {
        match self { Snd::Dec(d) => d.total_duration(), Snd::Buf(b) => b.total_duration() }
    }
}

pub struct Audio {
    _stream: Option<OutputStream>,
    handle: Option<OutputStreamHandle>,
    sinks: HashMap<u64, (Sink, f32)>,
    muted: bool,
    cache: HashMap<u64, Prepared>,
    next: u64,
}

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

impl Audio {
    pub fn new() -> Self {
        match OutputStream::try_default() {
            Ok((s, h)) => Self { _stream: Some(s), handle: Some(h), sinks: HashMap::new(), muted: false, cache: HashMap::new(), next: 1 },
            Err(e) => {
                eprintln!("[audio] no output device: {e}");
                Self { _stream: None, handle: None, sinks: HashMap::new(), muted: false, cache: HashMap::new(), next: 1 }
            }
        }
    }

    /// Plays encoded audio bytes. `volume` 0..1, `speed` 1.0 = normal (pitch effect).
    pub fn play(&mut self, bytes: &[u8], volume: f32, speed: f32) -> Option<u64> {
        let key = hash_bytes(bytes);
        let prepared = self.cache.entry(key).or_insert_with(|| prepare(bytes));
        let snd = match prepared {
            Prepared::Raw(b) => Snd::Dec(Decoder::new(Cursor::new(b.to_vec())).ok()?),
            Prepared::Pcm { channels, rate, data } => Snd::Buf(SamplesBuffer::new(*channels, *rate, data.to_vec())),
            Prepared::Fail => return None,
        };
        let handle = self.handle.as_ref()?;
        let sink = Sink::try_new(handle).ok()?;
        let volume = volume.max(0.0);
        sink.set_volume(if self.muted { 0.0 } else { volume });
        sink.append(snd.speed(speed.clamp(0.25, 4.0)));
        let id = self.next;
        self.next += 1;
        self.sinks.insert(id, (sink, volume));
        Some(id)
    }

    pub fn is_playing(&self, id: u64) -> bool {
        self.sinks.get(&id).map_or(false, |(s, _)| !s.empty())
    }

    pub fn stop_all(&mut self) {
        self.sinks.clear();
    }

    /// Forget sounds that finished.
    pub fn gc(&mut self) {
        self.sinks.retain(|_, (s, _)| !s.empty());
    }

    pub fn set_paused(&mut self, paused: bool) {
        for (s, _) in self.sinks.values() {
            if paused { s.pause() } else { s.play() }
        }
    }

    /// Mutes / unmutes everything (sounds already playing and future ones).
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        for (s, v) in self.sinks.values() {
            s.set_volume(if muted { 0.0 } else { *v });
        }
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }
}

// ------------------------------------------------------------- decoding

fn decodes(bytes: &[u8]) -> bool {
    match Decoder::new(Cursor::new(bytes.to_vec())) {
        Ok(mut d) => d.next().is_some(),
        Err(_) => false,
    }
}

fn prepare(bytes: &[u8]) -> Prepared {
    if decodes(bytes) {
        return Prepared::Raw(Arc::new(bytes.to_vec()));
    }
    if let Some((channels, rate, data)) = parse_wav(bytes) {
        if !data.is_empty() {
            return Prepared::Pcm { channels, rate, data: Arc::new(data) };
        }
    }
    if let Some(conv) = ffmpeg_convert(bytes) {
        if let Some((channels, rate, data)) = parse_wav(&conv) {
            if !data.is_empty() {
                return Prepared::Pcm { channels, rate, data: Arc::new(data) };
            }
        }
    }
    eprintln!("[audio] cannot decode this sound (unsupported or corrupted file)");
    Prepared::Fail
}

fn ffmpeg_convert(bytes: &[u8]) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir();
    let id = format!("{}_{}", std::process::id(), hash_bytes(bytes));
    let inp = dir.join(format!("rouille3_in_{id}.bin"));
    let out = dir.join(format!("rouille3_out_{id}.wav"));
    std::fs::write(&inp, bytes).ok()?;
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"]).arg(&inp)
        .args(["-f", "wav", "-acodec", "pcm_s16le"]).arg(&out)
        .stdout(Stdio::null()).stderr(Stdio::null()).status()
        .map_or(false, |s| s.success());
    let data = if ok { std::fs::read(&out).ok() } else { None };
    let _ = std::fs::remove_file(&inp);
    let _ = std::fs::remove_file(&out);
    data
}

fn r16(b: &[u8], o: usize) -> Option<u16> { Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?)) }
fn r32(b: &[u8], o: usize) -> Option<u32> { Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?)) }

/// Tolerant WAV reader: returns (channels, sample rate, interleaved i16 samples).
pub fn parse_wav(b: &[u8]) -> Option<(u16, u32, Vec<i16>)> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let (mut tag, mut ch, mut rate, mut align, mut bits, mut _spb) = (0u16, 0u16, 0u32, 0u16, 0u16, 0u16);
    let mut have_fmt = false;
    let mut data: Option<&[u8]> = None;
    let mut pos = 12usize;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = r32(b, pos + 4)? as usize;
        let start = pos + 8;
        if id == b"fmt " {
            tag = r16(b, start)?;
            ch = r16(b, start + 2)?;
            rate = r32(b, start + 4)?;
            align = r16(b, start + 12)?;
            bits = r16(b, start + 14)?;
            if tag == 0xFFFE && size >= 26 { tag = r16(b, start + 24)?; }
            if tag == 0x11 && size >= 20 { _spb = r16(b, start + 18)?; }
            have_fmt = true;
        } else if id == b"data" {
            let avail = b.len().saturating_sub(start);
            let len = if size == 0 || size > avail { avail } else { size };
            data = Some(&b[start..start + len]);
            break;
        }
        pos = start.checked_add(size)?.checked_add(size & 1)?;
    }
    let data = data?;
    if !have_fmt || ch == 0 || rate == 0 {
        return None;
    }
    let samples: Vec<i16> = match (tag, bits) {
        (1, 8) => data.iter().map(|&x| ((x as i16) - 128) << 8).collect(),
        (1, 16) => data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(),
        (1, 24) => data.chunks_exact(3).map(|c| i16::from_le_bytes([c[1], c[2]])).collect(),
        (1, 32) => data.chunks_exact(4).map(|c| (i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 16) as i16).collect(),
        (3, 32) => data.chunks_exact(4)
            .map(|c| (f32::from_le_bytes([c[0], c[1], c[2], c[3]]).clamp(-1.0, 1.0) * 32767.0) as i16).collect(),
        (3, 64) => data.chunks_exact(8)
            .map(|c| (f64::from_le_bytes(c.try_into().unwrap()).clamp(-1.0, 1.0) * 32767.0) as i16).collect(),
        (0x11, _) => adpcm_ima(data, ch as usize, align as usize)?,
        _ => return None,
    };
    Some((ch, rate, samples))
}

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80,
    88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544,
    598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749,
    3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487,
    12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX_ADJ: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

fn ima_nibble(pred: &mut i32, idx: &mut i32, nib: u8) -> i16 {
    let step = STEPS[*idx as usize];
    let mut diff = step >> 3;
    if nib & 1 != 0 { diff += step >> 2 }
    if nib & 2 != 0 { diff += step >> 1 }
    if nib & 4 != 0 { diff += step }
    if nib & 8 != 0 { diff = -diff }
    *pred = (*pred + diff).clamp(-32768, 32767);
    *idx = (*idx + INDEX_ADJ[(nib & 7) as usize]).clamp(0, 88);
    *pred as i16
}

/// IMA / DVI ADPCM in a WAV container (what Scratch's own sound library uses).
fn adpcm_ima(data: &[u8], ch: usize, align: usize) -> Option<Vec<i16>> {
    if ch == 0 || align < 4 * ch {
        return None;
    }
    let mut out = Vec::new();
    for block in data.chunks(align) {
        if block.len() < 4 * ch {
            break;
        }
        let mut pred = vec![0i32; ch];
        let mut idx = vec![0i32; ch];
        let mut chans: Vec<Vec<i16>> = vec![vec![]; ch];
        for c in 0..ch {
            pred[c] = i16::from_le_bytes([block[4 * c], block[4 * c + 1]]) as i32;
            idx[c] = (block[4 * c + 2] as i32).clamp(0, 88);
            chans[c].push(pred[c] as i16);
        }
        for group in block[4 * ch..].chunks(4 * ch) {
            for c in 0..ch {
                let part = group.get(4 * c..(4 * c + 4).min(group.len())).unwrap_or(&[]);
                for byte in part {
                    for nib in [byte & 0x0F, byte >> 4] {
                        let s = ima_nibble(&mut pred[c], &mut idx[c], nib);
                        chans[c].push(s);
                    }
                }
            }
        }
        let n = chans.iter().map(|v| v.len()).min().unwrap_or(0);
        for i in 0..n {
            for c in 0..ch {
                out.push(chans[c][i]);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the files generated in /tmp/audiotest (skipped when absent).
    #[test]
    fn decodes_many_formats() {
        let dir = std::path::Path::new("/tmp/audiotest");
        if !dir.exists() {
            return;
        }
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
        names.sort();
        let mut failed = vec![];
        for p in names {
            let bytes = std::fs::read(&p).unwrap();
            let kind = match prepare(&bytes) {
                Prepared::Raw(b) => {
                    let n = Decoder::new(Cursor::new(b.to_vec())).unwrap().count();
                    format!("rodio ({n} samples)")
                }
                Prepared::Pcm { channels, rate, data } => format!("fallback ({} ch, {} Hz, {} samples)", channels, rate, data.len()),
                Prepared::Fail => { failed.push(p.clone()); "FAILED".into() }
            };
            println!("{:<22} -> {}", p.file_name().unwrap().to_string_lossy(), kind);
        }
        assert!(failed.is_empty(), "could not decode: {failed:?}");
    }
}
