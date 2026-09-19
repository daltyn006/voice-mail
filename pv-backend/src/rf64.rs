//! Minimal streaming RF64 / ds64 WAV writer + reader (24-bit archival takes).
//!
//! Why not `hound`: hound only emits standard RIFF (32-bit sizes). Past
//! 4 GiB the RIFF sizes wrap to zero and a crash before any post-convert
//! leaves an unrecoverable file. This writer emits `RF64` with a `ds64`
//! chunk from byte zero and patches sizes on `finalize()`; a killed take is
//! repairable from EOF (`repair_file`, run at boot).
//!
//! Format written (PCM, 24-bit, native rate/channels):
//! `RF64[0xFFFFFFFF]WAVE ds64(28) fmt(16) data[0xFFFFFFFF] samples…`
//! `ds64 = { riff_size_u64, data_size_u64, sample_count_u64, table_len_u32=0 }`.
//! All integers little-endian. Files stay valid RF64 at every size including
//! <4 GiB, so readers must accept RF64 always (see `TakeSpec::open`).

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Split a part before 32-bit sizes could ever wrap: 2 h of frames is the
/// UX trigger; this byte ceiling is the safety trigger (whichever first).
pub const SPLIT_DATA_BYTES: u64 = 3_800_000_000;
/// 10-minute pre-split warning threshold helper lives in `record.rs`
/// (frames-based); this module only reports counts.

fn w32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn w64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn w16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn r32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn r64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}
fn r16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

/// Streaming RF64 writer. Owned by `TakeWriter` in `record.rs`.
pub struct Rf64Writer {
    out: Option<BufWriter<File>>,
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub data_bytes: u64,
    ds64_pos: u64,
    failed: Option<String>,
}

impl Rf64Writer {
    pub fn create(path: &Path, sample_rate: u32, channels: u16) -> Result<Self, String> {
        if sample_rate == 0 || channels == 0 {
            return Err("bad WAV spec (rate/channels)".to_string());
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        let f = File::create(path).map_err(|e| e.to_string())?;
        let mut out = BufWriter::with_capacity(1 << 20, f);
        // RF64 header with unknown sizes (0xFFFFFFFF sentinels + ds64).
        let mut h = Vec::with_capacity(104);
        h.extend_from_slice(b"RF64");
        w32(&mut h, 0xFFFF_FFFF);
        h.extend_from_slice(b"WAVE");
        h.extend_from_slice(b"ds64");
        w32(&mut h, 28); // chunk bytes
        let ds64_pos = h.len() as u64;
        w64(&mut h, 0xFFFF_FFFF_FFFF_FFFF); // riff_size (patched on finalize)
        w64(&mut h, 0xFFFF_FFFF_FFFF_FFFF); // data_size
        w64(&mut h, 0xFFFF_FFFF_FFFF_FFFF); // sample_count
        w32(&mut h, 0); // table_length
        h.extend_from_slice(b"fmt ");
        w32(&mut h, 16);
        w16(&mut h, 1); // PCM
        w16(&mut h, channels);
        w32(&mut h, sample_rate);
        w32(&mut h, sample_rate * channels as u32 * 3); // byte rate (24-bit)
        w16(&mut h, channels * 3); // block align
        w16(&mut h, 24); // bits
        h.extend_from_slice(b"data");
        w32(&mut h, 0xFFFF_FFFF); // RF64 sentinel (real size lives in ds64)
        out.write_all(&h).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
        Ok(Rf64Writer {
            out: Some(out),
            path: path.to_path_buf(),
            sample_rate,
            channels,
            frames: 0,
            data_bytes: 0,
            ds64_pos,
            failed: None,
        })
    }

    /// Append interleaved i24 samples (3 bytes LE each). Records the first
    /// IO error (disk-full) and ignores later pushes — caller checks `failed()`.
    pub fn push_i24(&mut self, samples: &[i32]) {
        if self.failed.is_some() {
            return;
        }
        let Some(out) = self.out.as_mut() else {
            return;
        };
        // Stack buffer per call would blow on huge blocks; stream in 64k.
        let mut buf = [0u8; 3 * 1024];
        let mut pending = 0;
        let mut flush_err: Option<String> = None;
        for &s in samples {
            let v = s.clamp(-8_388_608, 8_388_607);
            buf[pending * 3] = (v & 0xFF) as u8;
            buf[pending * 3 + 1] = ((v >> 8) & 0xFF) as u8;
            buf[pending * 3 + 2] = ((v >> 16) & 0xFF) as u8;
            pending += 1;
            if pending == 1024 {
                if let Err(e) = out.write_all(&buf[..pending * 3]) {
                    flush_err = Some(e.to_string());
                    break;
                }
                self.data_bytes += (pending * 3) as u64;
                pending = 0;
            }
        }
        if flush_err.is_none() && pending > 0 {
            if let Err(e) = out.write_all(&buf[..pending * 3]) {
                flush_err = Some(e.to_string());
            } else {
                self.data_bytes += (pending * 3) as u64;
            }
        }
        if let Some(e) = flush_err {
            self.failed = Some(format!("disk write failed ({e}) — take finalized, halting"));
            return;
        }
        self.frames += (samples.len() / self.channels.max(1) as usize) as u64;
        // Periodic flush so a killed take keeps everything up to ~1 s ago.
        if self.frames % (self.sample_rate as u64 * 2 + 1) < 1024 {
            let _ = out.flush();
        }
    }

    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// Patch ds64 + flush. Never deletes: a finalized prefix is always kept.
    pub fn finalize(&mut self) -> Result<(), String> {
        if let Some(e) = self.failed.clone() {
            let _ = self.patch_sizes_best_effort();
            return Err(e);
        }
        self.patch_sizes_best_effort()
    }

    fn patch_sizes_best_effort(&mut self) -> Result<(), String> {
        let Some(out) = self.out.as_mut() else {
            return Ok(());
        };
        out.flush().map_err(|e| e.to_string())?;
        let file = out.get_mut();
        let riff_size = 80 + self.data_bytes; // file_len - 8
        let data_size = self.data_bytes;
        let samples = self.frames * self.channels.max(1) as u64;
        file.seek(SeekFrom::Start(self.ds64_pos))
            .map_err(|e| e.to_string())?;
        file.write_all(&riff_size.to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.write_all(&data_size.to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.write_all(&samples.to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Parsed take header (RIFF legacy + RF64), 24-bit PCM focus but reports any
/// PCM/float spec so callers can reject cleanly.
#[derive(Clone, Debug)]
pub struct TakeSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub audio_format: u16, // 1 = PCM int, 3 = IEEE float
    pub data_bytes: u64,
    pub frames: u64,
    pub data_offset: u64,
}

fn parse_header(path: &Path) -> Result<TakeSpec, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).map_err(|_| "not a WAV file".to_string())?;
    let rf64 = &head[0..4] == b"RF64";
    let riff = &head[0..4] == b"RIFF";
    if !(rf64 || riff) || &head[8..12] != b"WAVE" {
        return Err("not a WAV file".to_string());
    }
    let mut fmt_rate = 0u32;
    let mut fmt_ch = 0u16;
    let mut fmt_bits = 0u16;
    let mut fmt_audio = 0u16;
    let mut ds64_data: Option<u64> = None;
    let mut data_pos = 0u64;
    let mut data_len32: u64 = 0;
    let mut data_len64: Option<u64> = None;
    loop {
        let mut ch = [0u8; 8];
        if f.read_exact(&mut ch).is_err() {
            break;
        }
        let len32 = r32(&ch[4..8]) as u64;
        let pos = f.stream_position().map_err(|e| e.to_string())?;
        if &ch[0..4] == b"ds64" {
            let mut d = vec![0u8; len32.min(32) as usize];
            f.read_exact(&mut d).map_err(|_| "truncated ds64".to_string())?;
            if d.len() >= 24 {
                let _riff = r64(&d[0..8]);
                ds64_data = Some(r64(&d[8..16]));
                let _samples = r64(&d[16..24]);
            }
            if len32 > d.len() as u64 {
                f.seek(SeekFrom::Current((len32 - d.len() as u64) as i64))
                    .map_err(|e| e.to_string())?;
            }
        } else if &ch[0..4] == b"fmt " {
            let mut d = vec![0u8; len32.min(40) as usize];
            f.read_exact(&mut d).map_err(|_| "truncated fmt".to_string())?;
            if d.len() < 16 {
                return Err("truncated fmt".to_string());
            }
            fmt_audio = r16(&d[0..2]);
            fmt_ch = r16(&d[2..4]);
            fmt_rate = r32(&d[4..8]);
            fmt_bits = r16(&d[14..16]);
            if len32 > d.len() as u64 {
                f.seek(SeekFrom::Current((len32 - d.len() as u64) as i64))
                    .map_err(|e| e.to_string())?;
            }
        } else if &ch[0..4] == b"data" {
            data_pos = pos;
            data_len32 = len32;
            if rf64 && len32 == 0xFFFF_FFFF {
                data_len64 = ds64_data;
            }
            break;
        } else {
            // Skip unknown chunks (fact, LIST, cue…), word-aligned.
            let skip = len32 + (len32 & 1);
            f.seek(SeekFrom::Current(skip as i64))
                .map_err(|e| e.to_string())?;
        }
        if data_pos != 0 {
            break;
        }
    }
    if fmt_ch == 0 || fmt_rate == 0 {
        return Err("WAV missing fmt chunk".to_string());
    }
    if data_pos == 0 {
        return Err("WAV missing data chunk".to_string());
    }
    // Resolve data length: ds64 wins for RF64; validate against the file.
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut data_bytes = data_len64.unwrap_or(data_len32);
    if data_bytes == 0xFFFF_FFFF_FFFF_FFFF || data_bytes + data_pos > file_len {
        // Placeholder (crashed take) or lying header: trust EOF.
        data_bytes = file_len.saturating_sub(data_pos);
    }
    let bytes_per_frame = (fmt_ch as u64) * ((fmt_bits as u64 + 7) / 8).max(1);
    let frames = if bytes_per_frame > 0 {
        data_bytes / bytes_per_frame
    } else {
        0
    };
    Ok(TakeSpec {
        sample_rate: fmt_rate,
        channels: fmt_ch,
        bits: fmt_bits,
        audio_format: fmt_audio,
        data_bytes,
        frames,
        data_offset: data_pos,
    })
}

/// Take length in whole seconds (cheap header open, no decode).
pub fn take_secs(path: &Path) -> u64 {
    let Ok(spec) = parse_header(path) else {
        return 0;
    };
    if spec.sample_rate == 0 {
        return 0;
    }
    spec.frames / spec.sample_rate as u64
}

/// Repair a crashed RF64 take (placeholder ds64 sizes) from EOF.
/// Returns true when bytes were patched.
pub fn repair_file(path: &Path) -> bool {
    let mut f = match File::options().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_err() || &head[0..4] != b"RF64" {
        return false;
    }
    // Locate ds64 + data the same way as parse_header, then compare.
    let spec = match parse_header(path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let data_bytes = file_len.saturating_sub(spec.data_offset);
    if data_bytes == spec.data_bytes {
        // Check the on-disk ds64 placeholders: if already patched, nothing to do.
        let mut probe = [0u8; 24];
        // ds64 payload starts at 12 + 8 = 20.
        if f.seek(SeekFrom::Start(20)).is_err() || f.read_exact(&mut probe).is_err() {
            return false;
        }
        if r64(&probe[8..16]) == data_bytes {
            return false;
        }
    }
    if f.seek(SeekFrom::Start(20)).is_err() {
        return false;
    }
    let riff_size = file_len.saturating_sub(8);
    let samples = spec.frames * spec.channels.max(1) as u64;
    let ok = f.write_all(&riff_size.to_le_bytes()).is_ok()
        && f.write_all(&data_bytes.to_le_bytes()).is_ok()
        && f.write_all(&samples.to_le_bytes()).is_ok();
    let _ = f.flush();
    ok
}

/// Repair every `.wav` under `dir`. Returns repaired file names.
pub fn scan_and_repair(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            if repair_file(&p) {
                out.push(p.file_name().and_then(|s| s.to_str()).unwrap_or("take.wav").to_string());
            }
        }
    }
    out
}

/// Sequential 24-bit PCM frame reader (RIFF legacy + RF64). Tolerant of the
/// crash-placeholder form via `parse_header`'s EOF rule.
pub struct TakeReader {
    file: File,
    pub spec: TakeSpec,
    frames_read: u64,
}

impl TakeReader {
    pub fn open(path: &Path) -> Result<Self, String> {
        let spec = parse_header(path)?;
        if spec.audio_format != 1 || spec.bits != 24 {
            return Err("only 24-bit PCM takes are supported".to_string());
        }
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(spec.data_offset))
            .map_err(|e| e.to_string())?;
        Ok(TakeReader {
            file,
            spec,
            frames_read: 0,
        })
    }

    pub fn total_frames(&self) -> u64 {
        self.spec.frames
    }

    /// Skip `n` frames (playback resume). Sequential — no index seek needed.
    pub fn skip_frames(&mut self, mut n: u64) -> Result<(), String> {
        let stride = self.spec.channels.max(1) as u64 * 3;
        let mut buf = [0u8; 3 * 512];
        while n > 0 {
            let want = n.min(512);
            let bytes = (want * stride) as usize;
            let mut got = 0;
            while got < bytes {
                let end = (got + buf.len()).min(bytes);
                match self.file.read(&mut buf[got..end]) {
                    Ok(0) => return Ok(()),
                    Ok(k) => got += k,
                    Err(e) => return Err(e.to_string()),
                }
            }
            self.frames_read += want;
            n -= want;
        }
        Ok(())
    }

    /// Read up to `frames.len()` frames as f32 mono-mapped later by caller;
    /// returns frames actually read. Each frame = channels × i24.
    pub fn read_frames_f32(&mut self, frames: &mut [Vec<f32>], nch: usize) -> Result<usize, String> {
        let fch = self.spec.channels.max(1) as usize;
        let stride = fch * 3;
        let mut raw = vec![0u8; stride * frames.len().max(1)];
        let mut got_frames = 0;
        for slot in frames.iter_mut() {
            if self.frames_read >= self.spec.frames {
                break;
            }
            let mut got = 0;
            while got < stride {
                match self.file.read(&mut raw[got..stride]) {
                    Ok(0) => break,
                    Ok(k) => got += k,
                    Err(e) => return Err(e.to_string()),
                }
            }
            if got < stride {
                break;
            }
            slot.clear();
            slot.reserve(nch.max(fch));
            for c in 0..fch {
                let v = (raw[c * 3] as i32)
                    | ((raw[c * 3 + 1] as i32) << 8)
                    | ((raw[c * 3 + 2] as i32) << 16);
                let v = if v & 0x800000 != 0 { v | !0xFFFFFF } else { v };
                slot.push(v as f32 / 8_388_608.0);
            }
            // Map to output channels (duplicate mono / truncate).
            if nch > fch {
                while slot.len() < nch {
                    slot.push(slot[0]);
                }
            }
            self.frames_read += 1;
            got_frames += 1;
        }
        Ok(got_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf64_roundtrip_repairs_from_eof() {
        let p = std::env::temp_dir().join(format!("pv-rf64-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut w = Rf64Writer::create(&p, 16000, 1).unwrap();
        w.push_i24(&[0, 8_388_607, -8_388_608, 12345]);
        assert!(w.failed().is_none());
        w.finalize().unwrap();
        let spec = parse_header(&p).unwrap();
        assert_eq!(spec.frames, 4);
        assert_eq!(take_secs(&p), 0); // 4 frames @16kHz < 1 s
        // Simulate a crash: clobber ds64 data_size with the placeholder, repair.
        {
            let mut f = File::options().read(true).write(true).open(&p).unwrap();
            f.seek(SeekFrom::Start(28)).unwrap();
            f.write_all(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes()).unwrap();
            f.flush().unwrap();
        }
        assert!(repair_file(&p));
        let back = parse_header(&p).unwrap();
        assert_eq!(back.frames, 4);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn legacy_riff_still_parses() {
        // Minimal 16-bit mono RIFF: 2 frames.
        let p = std::env::temp_dir().join(format!("pv-riff-{}", std::process::id()));
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&36u32.to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&16000u32.to_le_bytes());
        b.extend_from_slice(&32000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 4]);
        std::fs::write(&p, &b).unwrap();
        let spec = parse_header(&p).unwrap();
        assert_eq!(spec.frames, 2);
        let _ = std::fs::remove_file(&p);
    }
}
