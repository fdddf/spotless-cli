//! Path helpers, primarily `~` expansion and home-dir lookup.
//!
//! The home directory is resolved once via [`real_home`], but tests and the
//! Tauri layer can override it through the `MACCLEANER_HOME` environment
//! variable so scans can be pointed at a fixture tree.

use std::path::{Path, PathBuf};

/// The user's actual home directory, read from the password database rather
/// than the environment.
///
/// `dirs::home_dir` reads `$HOME`, which the App Store build must not trust:
/// the sandbox rewrites it to the app's container
/// (`~/Library/Containers/<id>/Data`). Every `~/…` rule in `src-tauri/rules`
/// would then expand inside that container and quietly scan an empty tree —
/// not an error, just nothing found, which is the worst kind of wrong.
///
/// `getpwuid` is not redirected by the sandbox, so it still reports the real
/// path. Learning the path is not the same as being allowed to read it: the
/// sandboxed build still needs a user-granted, security-scoped bookmark before
/// any of these directories open. This function only fixes *where* to look.
///
/// Falls back to `dirs::home_dir` if the lookup fails, which cannot happen for
/// a logged-in user but is not worth a panic.
fn real_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;

        // SAFETY: `getpwuid` returns a pointer into a static buffer owned by
        // libc, valid until the next call in this thread. The `CStr` is copied
        // into an owned `PathBuf` before returning, so nothing outlives it.
        unsafe {
            let passwd = libc::getpwuid(libc::getuid());
            if !passwd.is_null() && !(*passwd).pw_dir.is_null() {
                let dir = CStr::from_ptr((*passwd).pw_dir);
                if !dir.to_bytes().is_empty() {
                    return Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())));
                }
            }
        }
    }
    dirs::home_dir()
}

/// The effective home directory. Honors `MACCLEANER_HOME` when set (used by
/// tests and to point a scan at a fixture tree), otherwise [`real_home`].
pub fn home_dir() -> Option<PathBuf> {
    if let Ok(overridden) = std::env::var("MACCLEANER_HOME") {
        if !overridden.is_empty() {
            return Some(PathBuf::from(overridden));
        }
    }
    real_home()
}

/// Expand a leading `~` (or `~/...`) to the home directory. Paths without a
/// leading `~` are returned unchanged.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Convenience: expand a tilde path and return it as a `Path`-owning `PathBuf`.
pub fn resolve(path: &str) -> PathBuf {
    expand_tilde(path)
}

/// True if `path` currently exists on disk (following symlinks).
pub fn exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `MACCLEANER_HOME` is process-global; serialize tests that mutate it so
    // they don't race under the parallel test runner.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn expands_bare_tilde() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("MACCLEANER_HOME", "/home/test");
        assert_eq!(expand_tilde("~"), PathBuf::from("/home/test"));
        std::env::remove_var("MACCLEANER_HOME");
    }

    #[test]
    fn expands_tilde_prefix() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("MACCLEANER_HOME", "/home/test");
        assert_eq!(
            expand_tilde("~/Library/Caches"),
            PathBuf::from("/home/test/Library/Caches")
        );
        std::env::remove_var("MACCLEANER_HOME");
    }

    #[test]
    fn real_home_is_a_real_absolute_path() {
        // The sandbox regression this guards: `$HOME` points at the container,
        // `getpwuid` does not. Asserting the two agree would fail under the
        // sandbox by design, so assert only what holds everywhere — an
        // absolute path that exists and is not a container.
        let home = real_home().expect("a logged-in user has a home directory");
        assert!(home.is_absolute(), "{home:?} is not absolute");
        assert!(home.exists(), "{home:?} does not exist");
        assert!(
            !home.to_string_lossy().contains("/Library/Containers/"),
            "{home:?} is a sandbox container, not the real home"
        );
    }

    #[test]
    fn leaves_absolute_paths_untouched() {
        assert_eq!(
            expand_tilde("/Library/Caches"),
            PathBuf::from("/Library/Caches")
        );
    }
}
