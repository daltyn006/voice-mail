//! Install/data locations. Rule: the exe dir is read-only truth (portable +
//! per-machine installs); everything writable lives under the per-user data
//! dir so standard users never hit "access denied".

use std::path::PathBuf;
use std::sync::Mutex;

static MODELS_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);
static REVIEWS_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);
static OUTDIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Serializes tests that touch the process-global models-dir override
/// (dirs + models suites share this lock; otherwise a test run can leak
/// one test's override into another — or, worse, into real user data).
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Pin a custom models directory (Models page "Browse…"). Cleared with
/// [`clear_models_dir_override`]. Everything derived from [`models_dir`]
/// (downloads, imports, manifest, status) follows automatically.
pub fn set_models_dir_override(dir: PathBuf) {
    if let Ok(mut guard) = MODELS_OVERRIDE.lock() {
        *guard = Some(dir);
    }
}

/// Forget a custom models directory; back to the default home.
pub fn clear_models_dir_override() {
    if let Ok(mut guard) = MODELS_OVERRIDE.lock() {
        *guard = None;
    }
}

/// Pin a custom review-drafts directory (Settings → Storage).
pub fn set_reviews_dir_override(dir: PathBuf) {
    if let Ok(mut guard) = REVIEWS_OVERRIDE.lock() {
        *guard = Some(dir);
    }
}

/// Forget the custom drafts dir; back to `<data>/reviews`.
pub fn clear_reviews_dir_override() {
    if let Ok(mut guard) = REVIEWS_OVERRIDE.lock() {
        *guard = None;
    }
}

/// Pin a default output directory (Settings → Storage).
pub fn set_outdir_override(dir: PathBuf) {
    if let Ok(mut guard) = OUTDIR_OVERRIDE.lock() {
        *guard = Some(dir);
    }
}

/// Forget the output override; back to the per-user default home.
pub fn clear_outdir_override() {
    if let Ok(mut guard) = OUTDIR_OVERRIDE.lock() {
        *guard = None;
    }
}

fn models_override() -> Option<PathBuf> {
    MODELS_OVERRIDE.lock().ok().and_then(|g| g.clone())
}

/// Directory holding the running executable.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default()
}

/// Writable per-user home: `%LOCALAPPDATA%/voice mail`.
/// Falls back to the exe dir when the variable is absent (portable/dev).
pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|v| PathBuf::from(v).join("voice mail"))
        .unwrap_or_else(exe_dir)
}

/// Writable model home (downloads land here, never beside the exe).
/// Honors [`set_models_dir_override`] when pinned.
pub fn models_dir() -> PathBuf {
    if let Some(custom) = models_override() {
        return custom;
    }
    data_dir().join("models")
}

/// Default output home for finished `.md` files.
/// Honors [`set_outdir_override`] when pinned in Settings.
pub fn output_dir() -> PathBuf {
    if let Ok(g) = OUTDIR_OVERRIDE.lock() {
        if let Some(custom) = g.clone() {
            return custom;
        }
    }
    data_dir().join("output")
}

/// Review-drafts home (`<data>/reviews` unless overridden in Settings).
/// The AI (Store) creates per-output folders + `index.json` here on demand.
pub fn reviews_dir() -> PathBuf {
    if let Ok(g) = REVIEWS_OVERRIDE.lock() {
        if let Some(custom) = g.clone() {
            return custom;
        }
    }
    data_dir().join("reviews")
}

/// Day/Class memory database path.
pub fn db_path() -> PathBuf {
    data_dir().join("app.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_paths_nest_under_data_dir() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_models_dir_override();
        assert_eq!(models_dir().file_name().unwrap(), "models");
        assert_eq!(output_dir().file_name().unwrap(), "output");
        assert_eq!(db_path().file_name().unwrap(), "app.db");
        assert!(models_dir().starts_with(data_dir()));
    }

    #[test]
    fn models_override_wins_and_clears() {
        let _guard = TEST_LOCK.lock().unwrap();
        let custom = std::env::temp_dir().join("pv-models-custom");
        set_models_dir_override(custom.clone());
        assert_eq!(models_dir(), custom);
        clear_models_dir_override();
        assert!(models_dir().starts_with(data_dir()));
    }
}
