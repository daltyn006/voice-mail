//! Voice recording backend (Record page).
//!
//! Capture via `cpal` (WASAPI on Windows, CoreAudio/ALSA elsewhere) at the
//! device's native sample rate; archival takes are **24-bit RF64** written
//! streaming (see `rf64.rs`) so even a killed take stays repairable up to the
//! last flushed block. The 16 kHz mono STT feed + adaptive denoise are derived
//! later in the core — the archival take is never processed.
//!
//! Videos are NEVER touched here; this module only ever writes under
//! `<data>/recordings/`.

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Peak blocks retained for the live waveform (UI polls, oldest dropped).
pub const PEAK_CAP: usize = 240;

/// Directory holding finished + in-progress takes.
pub fn recordings_dir() -> PathBuf {
    crate::dirs::data_dir().join("recordings")
}

/// Fresh take path: `Rec <YYYY-MM-DD> <HHMMSS>.wav`, uniquified with a
/// counter so double-takes within one second never collide.
pub fn new_take_path() -> PathBuf {
    let dir = recordings_dir();
    let _ = std::fs::create_dir_all(&dir);
    let stamp = take_stamp();
    for n in 0..1000 {
        let name = if n == 0 {
            format!("Rec {stamp}.wav")
        } else {
            format!("Rec {stamp} ({n}).wav")
        };
        let p = dir.join(&name);
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("Rec {stamp} {}.wav", std::process::id()))
}

fn take_stamp() -> String {
    // Local-time formatting without new deps: fall back to epoch seconds
    // rendered as a stable sortable stem when the clock is odd.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYY-MM-DD HHMMSS via days arithmetic (civil-from-days, Hinnant).
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Human `MM:SS` / `H:MM:SS` length from a sample count + rate. Pure.
pub fn fmt_len(samples: u64, rate: u32) -> String {
    let secs = if rate > 0 { samples / rate as u64 } else { 0 };
    fmt_secs(secs)
}

/// Human length from whole seconds. Pure.
pub fn fmt_secs(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

fn f32_to_i24(v: f32) -> i32 {
    const MAX: f32 = 8_388_607.0;
    (v.clamp(-1.0, 1.0) * MAX).round() as i32
}

/// Streaming 24-bit RF64 take writer. Hardware-independent: the cpal callback
/// pushes blocks; the UI polls `peaks()` for the live waveform.
///
/// RF64 (not hound): the `ds64` chunk is present from byte zero so a killed
/// take stays repairable from EOF (`rf64::repair_file`, run at boot).
/// Parts split gaplessly at min(2 h, ~3.8 GiB); see `split_frames_for`.
pub struct TakeWriter {
    writer: Option<crate::rf64::Rf64Writer>,
    pub samples: u64,
    pub sample_rate: u32,
    pub channels: u16,
    peaks: Vec<f32>,
    pub peak_hold: f32,
    pub clips: u64,
    failed: Option<String>,
}

/// Frames of active recording before a gapless part split (2 h cap).
pub fn split_frames_for(sample_rate: u32) -> u64 {
    (sample_rate.max(8000) as u64) * 7200
}

/// Frames remaining before the split (saturating). Used for the 10-minute
/// yellow warning in the Record view.
pub fn frames_until_split(samples: u64, sample_rate: u32) -> u64 {
    split_frames_for(sample_rate).saturating_sub(samples)
}

/// True within the final 10 minutes before a split.
pub fn split_warning_due(samples: u64, sample_rate: u32) -> bool {
    frames_until_split(samples, sample_rate) <= (sample_rate.max(8000) as u64) * 600
}

/// Part file path: `Rec <stamp>.wav`, `Rec <stamp> (Part 2).wav`, …
pub fn part_path(dir: &std::path::Path, stamp: &str, part: u32) -> PathBuf {
    if part <= 1 {
        dir.join(format!("Rec {stamp}.wav"))
    } else {
        dir.join(format!("Rec {stamp} (Part {part}).wav"))
    }
}

impl TakeWriter {
    pub fn create(path: &PathBuf, sample_rate: u32, channels: u16) -> Result<Self, String> {
        let writer = crate::rf64::Rf64Writer::create(path, sample_rate, channels.max(1))?;
        Ok(TakeWriter {
            writer: Some(writer),
            samples: 0,
            sample_rate,
            channels: channels.max(1),
            peaks: Vec::new(),
            peak_hold: 0.0,
            clips: 0,
            failed: None,
        })
    }

    /// Push one interleaved f32 block from the capture thread.
    /// Disk-full is sticky: the first IO error finalizes what exists and all
    /// later pushes are ignored — caller surfaces `failed()` and halts.
    pub fn push_block(&mut self, block: &[f32]) {
        if self.failed.is_some() {
            return;
        }
        let ch = self.channels as usize;
        let Some(w) = self.writer.as_mut() else {
            return;
        };
        let mut peak = 0.0f32;
        let mut ints = Vec::with_capacity(block.len());
        for &v in block {
            ints.push(f32_to_i24(v));
            let a = v.abs();
            if a > peak {
                peak = a;
            }
        }
        w.push_i24(&ints);
        if let Some(e) = w.failed() {
            self.failed = Some(e.to_string());
            return;
        }
        if peak >= 0.999 {
            self.clips += 1;
        }
        if peak > self.peak_hold {
            self.peak_hold = peak;
        }
        self.samples += (block.len() / ch.max(1)) as u64;
        self.peaks.push(peak);
        if self.peaks.len() > PEAK_CAP {
            let drop = self.peaks.len() - PEAK_CAP;
            self.peaks.drain(..drop);
        }
    }

    /// Disk-full / IO failure message (sticky). `Some` means halt, finalize,
    /// and report — never retry against the same full disk.
    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref().or_else(|| {
            self.writer
                .as_ref()
                .and_then(|w| w.failed())
        })
    }

    pub fn data_bytes(&self) -> u64 {
        self.writer.as_ref().map(|w| w.data_bytes).unwrap_or(0)
    }

    /// Split trigger: 2 h of sample-counted frames OR the byte safety
    /// ceiling, whichever first. Pause time is excluded (samples only).
    pub fn split_due(&self) -> bool {
        self.samples >= split_frames_for(self.sample_rate)
            || self.data_bytes() >= crate::rf64::SPLIT_DATA_BYTES
    }

    pub fn peaks(&self) -> Vec<f32> {
        self.peaks.clone()
    }

    /// dBFS of the held peak (`-inf` when silent). Pure.
    pub fn peak_db(&self) -> String {
        if self.peak_hold <= 0.0 {
            "-inf dB".to_string()
        } else {
            format!("{:.0} dB", 20.0 * self.peak_hold.log10())
        }
    }

    pub fn finalize(&mut self) -> Result<(), String> {
        if let Some(w) = self.writer.as_mut() {
            w.finalize()?;
        }
        self.writer = None;
        if let Some(e) = self.failed.clone() {
            return Err(e);
        }
        Ok(())
    }
}

/// Display name for a device (best-effort; never fails).
fn dev_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| "(unnamed device)".to_string())
}

/// Input device names (best-effort: empty when the audio subsystem is down).
pub fn list_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            out.push(dev_name(&d));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Sample rates the named device reports (empty = query failed; caller falls
/// back to the device default). Pure query, no stream opened.
pub fn device_rates(name: &str) -> Vec<u32> {
    let host = cpal::default_host();
    let Ok(devs) = host.input_devices() else {
        return Vec::new();
    };
    for d in devs {
        if dev_name(&d) == name {
            let Ok(cfgs) = d.supported_input_configs() else {
                return Vec::new();
            };
            let mut rates: Vec<u32> = cfgs
                .flat_map(|c| {
                    let (lo, hi) = (c.min_sample_rate(), c.max_sample_rate());
                    // Report the range endpoints plus common voice rates inside.
                    [lo, hi, 16000, 44100, 48000]
                        .into_iter()
                        .filter(move |r| *r >= lo && *r <= hi)
                })
                .collect();
            rates.sort_unstable();
            rates.dedup();
            return rates;
        }
    }
    Vec::new()
}

struct RecShared {
    writer: Mutex<TakeWriter>,
    path: Mutex<PathBuf>,
    stamp: String,
    part: Mutex<u32>,
    finished: Mutex<Vec<PathBuf>>,
    paused: AtomicBool,
    stop: AtomicBool,
    failed: Mutex<Option<String>>,
}

/// Live capture handle. Dropping without `stop()` abandons the stream; the
/// WAV finalized so far stays playable. Prefer `stop()` (finalizes header).
pub struct Recorder {
    _stream: cpal::Stream,
    shared: Arc<RecShared>,
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub device: String,
}

impl Recorder {
    /// Start capturing from `device` (None = system default) at `rate`
    /// (None = device default). The device default channel layout is kept;
    /// archival stays native — STT downmix happens later in the core.
    pub fn start(device: Option<&str>, rate: Option<u32>) -> Result<Self, String> {
        let host = cpal::default_host();
        let dev = match device {
            Some(n) => {
                let mut found = None;
                let devs = host.input_devices().map_err(|e| e.to_string())?;
                for d in devs {
                    if dev_name(&d) == n {
                        found = Some(d);
                        break;
                    }
                }
                found.ok_or_else(|| format!("input device not found: {n}"))?
            }
            None => host
                .default_input_device()
                .ok_or_else(|| "no default input device — check Settings → Privacy → Microphone".to_string())?,
        };
        let dev_name = device
            .map(|s| s.to_string())
            .unwrap_or_else(|| dev_name(&dev));
        let cfg = match rate {
            Some(r) => {
                // Caller-picked rate: only what the driver reports is tried —
                // never invent an exclusive-mode rate (InvalidArgument class).
                let mut ok = false;
                if let Ok(cfgs) = dev.supported_input_configs() {
                    for c in cfgs {
                        if r >= c.min_sample_rate()
                            && r <= c.max_sample_rate()
                            && c.channels() >= 1
                        {
                            ok = true;
                            break;
                        }
                    }
                }
                if !ok {
                    return Err(format!("device '{dev_name}' does not support {r} Hz"));
                }
                // Re-query the exact config for channels at this rate.
                let mut picked: Option<cpal::SupportedStreamConfig> = None;
                if let Ok(cfgs) = dev.supported_input_configs() {
                    for c in cfgs {
                        if r >= c.min_sample_rate() && r <= c.max_sample_rate() {
                            picked = Some(c.with_sample_rate(r));
                            break;
                        }
                    }
                }
                picked.ok_or_else(|| format!("no channel layout at {r} Hz on '{dev_name}'"))?
            }
            None => dev
                .default_input_config()
                .map_err(|e| format!("cannot open input ({e}) — is another app using the mic?"))?,
        };
        let sample_rate = cfg.sample_rate();
        let channels = cfg.channels();
        let dir = recordings_dir();
        let _ = std::fs::create_dir_all(&dir);
        let stamp = take_stamp();
        // Uniquify the base stamp so double-takes within one second never collide.
        let mut base = stamp.clone();
        for n in 0..1000 {
            let cand = if n == 0 { stamp.clone() } else { format!("{stamp} ({n})") };
            if !part_path(&dir, &cand, 1).exists() {
                base = cand;
                break;
            }
        }
        let path = part_path(&dir, &base, 1);
        let writer = TakeWriter::create(&path, sample_rate, channels)?;
        let shared = Arc::new(RecShared {
            writer: Mutex::new(writer),
            path: Mutex::new(path.clone()),
            stamp: base,
            part: Mutex::new(1),
            finished: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            failed: Mutex::new(None),
        });
        let worker = Arc::clone(&shared);
        let err_text = Arc::new(Mutex::new(String::new()));
        let err_w = Arc::clone(&err_text);
        let stream = dev
            .build_input_stream(
                cfg.config(),
                move |data: &[f32], _| {
                    if worker.stop.load(Ordering::SeqCst) || worker.paused.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Ok(mut w) = worker.writer.lock() {
                        w.push_block(data);
                        if let Some(e) = w.failed() {
                            if let Ok(mut f) = worker.failed.lock() {
                                if f.is_none() {
                                    *f = Some(e.to_string());
                                }
                            }
                        }
                    }
                },
                move |e| {
                    if let Ok(mut s) = err_w.lock() {
                        *s = e.to_string();
                    }
                },
                None,
            )
            .map_err(|e| format!("cannot start capture ({e})"))?;
        stream.play().map_err(|e| format!("cannot start capture ({e})"))?;
        if let Ok(s) = err_text.lock() {
            if !s.is_empty() {
                return Err(format!("capture error: {s}"));
            }
        }
        Ok(Recorder {
            _stream: stream,
            shared,
            path,
            sample_rate,
            channels,
            device: dev_name,
        })
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
    }

    pub fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::SeqCst)
    }

    pub fn samples(&self) -> u64 {
        self.shared
            .writer
            .lock()
            .map(|w| w.samples)
            .unwrap_or(0)
    }

    pub fn peaks(&self) -> Vec<f32> {
        self.shared
            .writer
            .lock()
            .map(|w| w.peaks())
            .unwrap_or_default()
    }

    pub fn peak_db(&self) -> String {
        self.shared
            .writer
            .lock()
            .map(|w| w.peak_db())
            .unwrap_or_else(|_| "-inf dB".to_string())
    }

    pub fn clips(&self) -> u64 {
        self.shared.writer.lock().map(|w| w.clips).unwrap_or(0)
    }

    /// Sticky disk-full / IO failure from the capture thread (halt, finalize,
    /// report — never retry against a full disk).
    pub fn write_failed(&self) -> Option<String> {
        self.shared.failed.lock().ok().and_then(|f| f.clone())
    }

    /// Current 1-based part number (for the "Part N" status line).
    pub fn part(&self) -> u32 {
        self.shared.part.lock().map(|p| *p).unwrap_or(1)
    }

    /// Gapless rotation check, called from the UI poll (not the audio
    /// callback): when the active part hits min(2 h, ~3.8 GiB), finalize it
    /// and open Part N+1 without dropping blocks queued after the lock.
    /// Returns the finished part path when a rotation happened.
    pub fn poll_rotate(&self) -> Option<PathBuf> {
        let due = self
            .shared
            .writer
            .lock()
            .map(|w| w.split_due())
            .unwrap_or(false);
        if !due {
            return None;
        }
        let mut w = self.shared.writer.lock().ok()?;
        let mut part = self.shared.part.lock().ok()?;
        let mut cur = self.shared.path.lock().ok()?;
        let mut done = self.shared.finished.lock().ok()?;
        let finished_path = cur.clone();
        let _ = w.finalize();
        done.push(finished_path.clone());
        *part += 1;
        let dir = recordings_dir();
        let next = part_path(&dir, &self.shared.stamp, *part);
        match TakeWriter::create(&next, w.sample_rate, w.channels) {
            Ok(nw) => {
                let peak_hold = w.peak_hold;
                let clips = w.clips;
                *w = nw;
                w.peak_hold = peak_hold;
                w.clips = clips;
                *cur = next.clone();
                Some(finished_path)
            }
            Err(_) => None,
        }
    }

    /// Stop capture and finalize all parts. Returns every part file
    /// (finished rotations + the live tail) — the caller stages each as a
    /// standalone Input entry (FIFO-simple, no merged pseudo-jobs).
    pub fn stop(self) -> Result<Vec<PathBuf>, String> {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Dropping the stream first ends the callback; then finalize.
        let shared = self.shared.clone();
        let first_path = self.path.clone();
        drop(self._stream);
        let mut w = shared
            .writer
            .lock()
            .map_err(|_| "recorder lock poisoned".to_string())?;
        let res = w.finalize();
        drop(w);
        let mut parts = shared
            .finished
            .lock()
            .map(|f| f.clone())
            .unwrap_or_default();
        let cur = shared
            .path
            .lock()
            .map(|p| p.clone())
            .unwrap_or(first_path);
        if !parts.contains(&cur) {
            parts.push(cur);
        }
        // Surface a sticky disk-full even when finalize "succeeded".
        if let Some(e) = shared.failed.lock().ok().and_then(|f| f.clone()) {
            return Err(e);
        }
        res?;
        Ok(parts)
    }
}

/// Take length in whole seconds from the file header (cheap open, no decode).
/// RF64-aware (see `rf64::take_secs`); legacy hound-written RIFFs parse too.
pub fn take_secs(path: &PathBuf) -> u64 {
    crate::rf64::take_secs(path)
}

/// Boot crash-repair: rebuild placeholder ds64 sizes from EOF for every
/// `.wav` under the recordings dir. Returns repaired file names for the
/// "Recovered an unsent recording" notice. Silent scan, announced success.
pub fn repair_stale_takes() -> Vec<String> {
    crate::rf64::scan_and_repair(&recordings_dir())
}

struct PlayShared {
    reader: Mutex<crate::rf64::TakeReader>,
    file_rate: u32,
    file_ch: u16,
    out_rate: u32,
    out_ch: u16,
    /// Fractional file-frame cursor (zero-order-hold resample).
    pos: Mutex<f64>,
    /// File frames fully consumed from the reader (monotone).
    consumed: Mutex<u64>,
    /// Last fully-read file frame (re-emitted while the cursor lingers).
    carry: Mutex<Vec<f32>>,
    total_frames: u64,
    stop: AtomicBool,
    done: AtomicBool,
}

/// Playback handle for a finished take. Dropping stops the sound; the file
/// is untouched. Pause = drop + remember `pos_frames()`; resume = `play`
/// again from that offset.
pub struct Player {
    _stream: cpal::Stream,
    shared: Arc<PlayShared>,
    pub path: PathBuf,
}

impl Player {
    pub fn play(path: &PathBuf, offset_frames: u64) -> Result<Self, String> {
        let reader =
            crate::rf64::TakeReader::open(path).map_err(|e| format!("unreadable take ({e})"))?;
        let file_rate = reader.spec.sample_rate;
        let file_ch = reader.spec.channels;
        if file_rate == 0 || file_ch == 0 {
            return Err("unreadable take".to_string());
        }
        let total_frames = reader.total_frames();
        let host = cpal::default_host();
        let dev = host
            .default_output_device()
            .ok_or_else(|| "no output device found".to_string())?;
        let cfg = dev
            .default_output_config()
            .map_err(|e| format!("cannot open output ({e})"))?;
        let out_rate = cfg.sample_rate();
        let out_ch = cfg.channels();
        let shared = Arc::new(PlayShared {
            reader: Mutex::new(reader),
            file_rate,
            file_ch,
            out_rate,
            out_ch,
            pos: Mutex::new(offset_frames as f64),
            consumed: Mutex::new(0),
            carry: Mutex::new(vec![0.0; file_ch.max(1) as usize]),
            total_frames,
            stop: AtomicBool::new(false),
            done: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        // Prime past the resume offset (sequential read; no frame seek here).
        if offset_frames > 0 {
            if let Ok(mut r) = worker.reader.lock() {
                let _ = r.skip_frames(offset_frames);
            }
            if let Ok(mut p) = worker.pos.lock() {
                *p = offset_frames as f64;
            }
            if let Ok(mut c) = worker.consumed.lock() {
                *c = offset_frames;
            }
        }
        let stream = dev
            .build_output_stream(
                cfg.config(),
                move |data: &mut [f32], _| {
                    if worker.stop.load(Ordering::SeqCst) {
                        data.fill(0.0);
                        return;
                    }
                    let Ok(mut r) = worker.reader.lock() else {
                        data.fill(0.0);
                        return;
                    };
                    let Ok(mut pos) = worker.pos.lock() else {
                        data.fill(0.0);
                        return;
                    };
                    let Ok(mut consumed) = worker.consumed.lock() else {
                        data.fill(0.0);
                        return;
                    };
                    let Ok(mut carry) = worker.carry.lock() else {
                        data.fill(0.0);
                        return;
                    };
                    let step = worker.file_rate as f64 / worker.out_rate.max(1) as f64;
                    let fch = worker.file_ch.max(1) as usize;
                    let och = worker.out_ch.max(1) as usize;
                    let mut di = 0;
                    // Zero-order-hold resample: the cursor advances `step`
                    // file-frames per output frame; a new file frame is read
                    // only when the cursor passes unconsumed input.
                    while di < data.len() {
                        let idx = pos.floor() as u64;
                        if idx >= worker.total_frames {
                            worker.done.store(true, Ordering::SeqCst);
                            data[di..].fill(0.0);
                            break;
                        }
                        while *consumed <= idx {
                            let mut slot = vec![0.0f32; 1];
                            // Read one file frame (all channels) via the
                            // RF64 reader; EOF ends playback cleanly.
                            let mut one = [std::mem::take(&mut slot)];
                            match r.read_frames_f32(&mut one, fch) {
                                Ok(1) => {
                                    *carry = one[0].clone();
                                }
                                _ => {
                                    worker.done.store(true, Ordering::SeqCst);
                                    data[di..].fill(0.0);
                                    return;
                                }
                            }
                            *consumed += 1;
                        }
                        for o in 0..och {
                            if di >= data.len() {
                                break;
                            }
                            data[di] = if fch == 1 {
                                carry[0]
                            } else if o < fch {
                                carry[o]
                            } else {
                                carry[0]
                            };
                            di += 1;
                        }
                        *pos += step;
                    }
                },
                |_| {},
                None,
            )
            .map_err(|e| format!("cannot play ({e})"))?;
        stream.play().map_err(|e| format!("cannot play ({e})"))?;
        Ok(Player {
            _stream: stream,
            shared,
            path: path.clone(),
        })
    }

    pub fn is_done(&self) -> bool {
        self.shared.done.load(Ordering::SeqCst)
    }

    pub fn pos_frames(&self) -> u64 {
        self.shared
            .pos
            .lock()
            .map(|p| p.floor() as u64)
            .unwrap_or(0)
    }

    pub fn stop(self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_wav(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pv-rec-{tag}-{}.wav", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn take_paths_uniquify() {
        let a = new_take_path();
        std::fs::write(&a, b"").unwrap();
        let b = new_take_path();
        assert_ne!(a, b);
        assert_eq!(b.extension().and_then(|e| e.to_str()), Some("wav"));
        let _ = std::fs::remove_file(&a);
    }

    #[test]
    fn writer_roundtrips_24bit() {
        let p = tmp_wav("roundtrip");
        let mut w = TakeWriter::create(&p, 16000, 1).unwrap();
        // 0.5 s of 440 Hz sine.
        let block: Vec<f32> = (0..8000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 16000.0).sin() * 0.5)
            .collect();
        w.push_block(&block);
        assert_eq!(w.samples, 8000);
        assert!(w.failed().is_none());
        assert!(!w.peaks().is_empty());
        w.finalize().unwrap();
        let r = crate::rf64::TakeReader::open(&p).unwrap();
        assert_eq!(r.spec.bits, 24);
        assert_eq!(r.spec.sample_rate, 16000);
        assert_eq!(r.total_frames(), 8000);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn peaks_cap_and_clip_count() {
        let p = tmp_wav("peaks");
        let mut w = TakeWriter::create(&p, 8000, 1).unwrap();
        for _ in 0..(PEAK_CAP + 50) {
            w.push_block(&[0.1; 64]);
        }
        assert_eq!(w.peaks().len(), PEAK_CAP);
        assert_eq!(w.clips, 0);
        w.push_block(&[1.0; 64]);
        assert_eq!(w.clips, 1);
        assert!(w.peak_db().contains("dB"));
        w.finalize().unwrap();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn fmt_lens() {
        assert_eq!(fmt_len(96000, 16000), "00:06");
        assert_eq!(fmt_len(3600 * 16000 + 61 * 16000, 16000), "1:01:01");
        assert_eq!(fmt_len(0, 0), "00:00");
    }

    #[test]
    fn split_triggers_and_warns() {
        assert_eq!(split_frames_for(16000), 16000 * 7200);
        assert!(split_warning_due(split_frames_for(8000) - 100, 8000));
        assert!(!split_warning_due(100, 8000));
        assert_eq!(frames_until_split(10, 8000), 8000 * 7200 - 10);
    }

    #[test]
    fn device_queries_never_panic_headless() {
        // CI/headless boxes have no mic: must return empty, never panic.
        let _ = list_devices();
        let _ = device_rates("no-such-device");
    }
}
