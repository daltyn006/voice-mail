//! Backend-to-UI events. Everything crossing into the GPUI executor is one
//! of these; files are identified by **path** (stable), never FIFO index.

/// Pipeline stages, mirroring the core `PvStage` ABI (decode..error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Decode = 0,
    Transcribe = 1,
    Summarize = 2,
    Title = 3,
    Diff = 4,
    Done = 5,
    Error = 6,
}

impl Stage {
    pub fn from_i32(v: i32) -> Stage {
        match v {
            0 => Stage::Decode,
            1 => Stage::Transcribe,
            2 => Stage::Summarize,
            3 => Stage::Title,
            4 => Stage::Diff,
            5 => Stage::Done,
            _ => Stage::Error,
        }
    }

    /// Which UI column this stage feeds: `false` = Voice→Text, `true` = Text→Summary.
    pub fn summary_column(self) -> bool {
        (self as i32) >= (Stage::Summarize as i32) && (self as i32) <= (Stage::Diff as i32)
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Per-file pipeline progress. `aborted = true` means the core stopped
    /// this file at a safe point (no output written); the UI returns it to Input.
    File {
        file: String,
        stage: Stage,
        fraction: f32,
        message: String,
        aborted: bool,
    },
    /// Model download progress for one model id.
    Download {
        id: String,
        downloaded: u64,
        total: u64,
        done: bool,
        error: String,
    },
    /// Pre-run model validation (load + tiny discharge per backend).
    /// `stage` is "stt" or "llm"; `done` with empty `error` advances the
    /// phase, `done` with an error aborts the pending run.
    Validate {
        stage: String,
        done: bool,
        error: String,
    },
    /// Opt-in web research finished (worker thread, never the UI thread).
    /// `notes` counts fetched sources (zero is a normal offline degrade).
    /// `input` is the staged input id the notes belong to (phase-2 video
    /// flow: pass-1 transcript → research → pass-2 full job). Unknown ids
    /// (aborted meanwhile) are a no-op by construction, never a resurrect.
    Research {
        done: bool,
        notes: usize,
        input: u64,
    },
    /// Boot integrity pass finished (background worker, once per process).
    /// Carries human-readable warning lines (empty = all pinned models
    /// verified). Start blocks while the pass is in flight instead of
    /// hashing gigabytes synchronously.
    BootVerified {
        warnings: Vec<String>,
    },
    /// Manual update check finished (worker thread). Human-readable result
    /// for the status line ("Up to date" / "Update available" / error).
    UpdateCheck {
        message: String,
    },
    /// Background Ollama link-hash check finished (worker thread, spawned at
    /// link time — multi-GB blobs must never hash on the UI thread). `ok`
    /// means the blob hashes to its manifest digest; a mismatch drops the
    /// link (worker removes the manifest record) and `message` explains why.
    LinkVerified {
        id: String,
        ok: bool,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_mapping_and_columns() {
        assert_eq!(Stage::from_i32(0), Stage::Decode);
        assert_eq!(Stage::from_i32(5), Stage::Done);
        assert_eq!(Stage::from_i32(99), Stage::Error);
        assert!(!Stage::Transcribe.summary_column());
        assert!(Stage::Summarize.summary_column());
        assert!(!Stage::Done.summary_column());
    }
}
