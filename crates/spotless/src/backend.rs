//! Removal backends — the only code in this program that deletes anything.
//!
//! Every path still passes through [`SafetyGuard`](spotless_core::SafetyGuard)
//! before it reaches one of these; a backend is the *how*, never the *whether*.

use std::path::{Path, PathBuf};

use spotless_core::cleaner::RemovalBackend;

/// The default: move to the Trash, exactly the way Finder does.
///
/// `NsFileManager` rather than the crate's default method so trashed items keep
/// their "Put Back" information — an item Spotless removed should be
/// recoverable the same way one the user dragged there is.
pub struct TrashBackend;

impl RemovalBackend for TrashBackend {
    fn remove(&self, path: &Path) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            let mut ctx = trash::TrashContext::default();
            ctx.set_delete_method(DeleteMethod::NsFileManager);
            ctx.delete(path).map_err(|e| e.to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            trash::delete(path).map_err(|e| e.to_string())
        }
    }
}

/// Unlink outright. Used for `--permanent`, and for the handful of targets that
/// have nowhere to move to.
pub struct DeleteBackend;

impl RemovalBackend for DeleteBackend {
    fn remove(&self, path: &Path) -> Result<(), String> {
        // `symlink_metadata`, not `is_dir`: a symlink to a directory must be
        // unlinked, not recursively deleted through.
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            std::fs::remove_dir_all(path).map_err(|e| e.to_string())
        } else {
            std::fs::remove_file(path).map_err(|e| e.to_string())
        }
    }
}

/// Trash for almost everything, permanent delete under the roots that cannot be
/// trashed.
///
/// The Trash itself is the case this exists for. Asking for something already
/// in the Trash to be trashed does not fail — it shuffles the item around
/// inside the same folder and reclaims nothing, which would look like a
/// successful clean that freed no space.
pub struct RoutingBackend {
    /// Resolved roots whose contents must be deleted outright.
    pub permanent_roots: Vec<PathBuf>,
}

impl RemovalBackend for RoutingBackend {
    fn remove(&self, path: &Path) -> Result<(), String> {
        if self.permanent_roots.iter().any(|r| path.starts_with(r)) {
            DeleteBackend.remove(path)
        } else {
            TrashBackend.remove(path)
        }
    }
}

/// Whether a removal through `backend` lands somewhere recoverable, for the
/// sentence printed before the user commits.
pub fn describes_trash(permanent: bool) -> &'static str {
    if permanent {
        "deleted permanently"
    } else {
        "moved to the Trash"
    }
}
