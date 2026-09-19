//! pv-backend: framework-free backend for voice mail.
//!
//! Port of the ex-Tauri `main.rs` shell minus all Tauri coupling. The GPUI app
//! in `app/` drives this crate through plain Rust calls; progress flows back
//! over [`Event`] channels the UI forwards into its executor.
//!
//! Audit-bug ledger (each fixed here, not in UI code):
//! - stable file identity (path-keyed, never FIFO index),
//! - abort epochs (stale pre-abort DONE events are dropped),
//! - option lifecycle (no resurrection of removed files),
//! - single-lock shared state (no ABBA deadlock),
//! - output confinement (no traversal, extension whitelist),
//! - poison-safe locks (no `unwrap` on any `Mutex`).

pub mod catalog;
pub mod core_bridge;
pub mod diag;
pub mod dirs;
pub mod docs;
pub mod download;
pub mod drafts;
pub mod manifest;
pub mod merge;
pub mod models;
pub mod ollama;
pub mod prefs;
pub mod progress;
pub mod queue;
pub mod record;
pub mod paths;
pub mod research;
pub mod rf64;
pub mod share;
pub mod update;
pub mod verify;

pub use progress::Event;

/// M0 liveness probe (replaced by real health in M1 widening).
pub fn health() -> &'static str {
    "pv-backend ok"
}

#[cfg(test)]
mod tests {
    use super::health;

    #[test]
    fn health_reports_ok() {
        assert_eq!(health(), "pv-backend ok");
    }
}
