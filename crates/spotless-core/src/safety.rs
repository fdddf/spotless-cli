//! The safety layer: the guardrails that stand between a scan result and the
//! filesystem. Nothing in Spotless deletes anything without first passing a
//! path through [`SafetyGuard::validate`].
//!
//! Design rules:
//! - Deny by default for anything under a system-critical root.
//! - Every path is canonicalized (symlinks resolved) before it is judged, so a
//!   symlink cannot be used to escape an approved root.
//! - A path must live inside one of the approved roots to be removable.

use std::path::{Component, Path, PathBuf};

use crate::paths;

/// Reasons a path can be refused. Kept as a typed enum so callers and tests can
/// assert on exact causes rather than string-matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// Path is (or is inside) a hard-blocked system-critical location.
    DeniedRoot(PathBuf),
    /// Path resolves outside every approved root.
    OutsideApprovedRoots,
    /// Path is the user's home directory itself (never delete wholesale).
    HomeRoot,
    /// Path is empty or otherwise malformed.
    Malformed,
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefusalReason::DeniedRoot(p) => {
                write!(
                    f,
                    "path is within a protected system location: {}",
                    p.display()
                )
            }
            RefusalReason::OutsideApprovedRoots => {
                write!(f, "path is outside the approved cleaning roots")
            }
            RefusalReason::HomeRoot => {
                write!(f, "refusing to operate on the home directory itself")
            }
            RefusalReason::Malformed => write!(f, "path is empty or malformed"),
        }
    }
}

/// System-critical prefixes that must never be touched by any operation. A path
/// is denied if it equals or is nested under any of them.
fn system_deny_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/System"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/usr"),
        PathBuf::from("/etc"),
        PathBuf::from("/var/db"),
        PathBuf::from("/private/var/db"),
        PathBuf::from("/Applications"),
        PathBuf::from("/Library/Application Support/com.apple.TCC"),
    ]
}

/// User-data prefixes protected during general cleaning (caches/logs/etc.).
///
/// These are deliberately *not* applied when cleaning developer artifacts:
/// a directory literally named `node_modules` or `target` is safe to remove
/// even when it lives under Documents or Desktop, so blocking those roots would
/// only get in the way. Developer cleaning is instead gated by an exact
/// artifact-name check (see the app layer) plus [`system_deny_roots`].
fn user_data_deny_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = paths::home_dir() {
        for sub in ["Documents", "Desktop", "Pictures", "Library/Keychains"] {
            roots.push(home.join(sub));
        }
    }
    roots
}

/// Paths that must never be touched during general cleaning.
fn default_deny_roots() -> Vec<PathBuf> {
    let mut roots = system_deny_roots();
    roots.extend(user_data_deny_roots());
    roots
}

/// The set of roots under which cleaning is permitted at all. A path must be
/// inside one of these *and* not inside a deny root to be removable.
fn default_approved_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = paths::home_dir() {
        roots.push(home.join("Library/Caches"));
        roots.push(home.join("Library/Logs"));
        roots.push(home.join("Library/Application Support/CrashReporter"));
        roots.push(home.join("Library/Developer"));
        roots.push(home.join("Library/Containers"));
        roots.push(home.join("Downloads"));
        roots.push(home.join(".npm"));
        roots.push(home.join(".cache"));
        roots.push(home.join(".gradle"));
        roots.push(home.join(".cargo/registry/cache"));
    }
    roots.push(PathBuf::from("/Library/Caches"));
    roots.push(PathBuf::from("/Library/Logs"));
    roots
}

/// Validates paths against deny/approve rules before any deletion.
#[derive(Debug, Clone)]
pub struct SafetyGuard {
    deny_roots: Vec<PathBuf>,
    approved_roots: Vec<PathBuf>,
}

impl Default for SafetyGuard {
    fn default() -> Self {
        Self::new(default_deny_roots(), default_approved_roots())
    }
}

impl SafetyGuard {
    /// Construct a guard, resolving every root the same way [`validate`] resolves
    /// the path it judges.
    ///
    /// [`validate`](Self::validate) canonicalizes the candidate path (so symlinks
    /// are followed) before matching. If the roots were *not* resolved the same
    /// way, a canonical path like `/private/var/…` would never match an approved
    /// root written as `/var/…` — the two name the same directory but differ
    /// lexically. Resolving both sides keeps the comparison honest.
    fn new(deny_roots: Vec<PathBuf>, approved_roots: Vec<PathBuf>) -> Self {
        Self {
            deny_roots: deny_roots.iter().map(|p| resolve_root(p)).collect(),
            approved_roots: approved_roots.iter().map(|p| resolve_root(p)).collect(),
        }
    }

    /// Construct a guard with explicit roots. Primarily for tests; production
    /// code should use [`SafetyGuard::default`].
    pub fn with_roots(deny_roots: Vec<PathBuf>, approved_roots: Vec<PathBuf>) -> Self {
        Self::new(deny_roots, approved_roots)
    }

    /// A guard for developer-artifact cleaning: it denies only system-critical
    /// roots and approves nothing by default. The caller must
    /// [`approve_root`](Self::approve_root) each specific artifact path (after
    /// confirming its directory name is a recognized build/dependency artifact),
    /// so only those exact paths become removable.
    pub fn system_only() -> Self {
        Self::new(system_deny_roots(), Vec::new())
    }

    /// A guard for app uninstallation. Like [`system_only`](Self::system_only)
    /// but does **not** deny `/Applications`, so a specific `.app` bundle can be
    /// approved and removed. The caller must still confirm the path is an `.app`
    /// under an Applications directory before approving it.
    pub fn for_uninstall() -> Self {
        let deny = system_deny_roots()
            .into_iter()
            .filter(|p| p != Path::new("/Applications"))
            .collect();
        Self::new(deny, Vec::new())
    }

    /// Add an approved root (e.g. one derived from a loaded rule's path).
    ///
    /// The root is resolved (symlinks followed where it exists) so it matches the
    /// canonicalized paths [`validate`](Self::validate) judges — see [`new`](Self::new).
    pub fn approve_root(&mut self, root: PathBuf) {
        let root = resolve_root(&root);
        if !self.approved_roots.contains(&root) {
            self.approved_roots.push(root);
        }
    }

    /// Decide whether `path` may be removed.
    ///
    /// The path is normalized (and canonicalized when it exists on disk, to
    /// resolve symlinks) before judgment. Returns `Ok(canonical_path)` when the
    /// path is safe to remove, or `Err(reason)` otherwise.
    pub fn validate(&self, path: &Path) -> Result<PathBuf, RefusalReason> {
        if path.as_os_str().is_empty() {
            return Err(RefusalReason::Malformed);
        }

        // Resolve symlinks where possible; fall back to lexical normalization
        // for paths that don't exist (e.g. during dry-run planning of a path
        // that was already removed).
        let resolved = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => normalize_lexical(path),
        };

        if let Some(home) = paths::home_dir() {
            if resolved == home {
                return Err(RefusalReason::HomeRoot);
            }
        }

        for deny in &self.deny_roots {
            if is_within(&resolved, deny) {
                return Err(RefusalReason::DeniedRoot(deny.clone()));
            }
        }

        let approved = self
            .approved_roots
            .iter()
            .any(|root| is_within(&resolved, root));
        if !approved {
            return Err(RefusalReason::OutsideApprovedRoots);
        }

        Ok(resolved)
    }
}

/// Resolve a root for comparison: canonicalize it (following symlinks) when it
/// exists on disk, else fall back to lexical normalization.
///
/// This mirrors exactly how [`SafetyGuard::validate`] resolves the path it
/// judges, so both sides of [`is_within`] are expressed in the same terms.
fn resolve_root(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_lexical(path))
}

/// Returns true if `path` equals `root` or is nested beneath it.
fn is_within(path: &Path, root: &Path) -> bool {
    let path = normalize_lexical(path);
    let root = normalize_lexical(root);
    path == root || path.starts_with(&root)
}

/// Lexically normalize a path: strip `.` components and resolve `..` without
/// touching the filesystem. Used as a fallback and to defend against `..`
/// traversal in rule-supplied paths.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> SafetyGuard {
        SafetyGuard::with_roots(
            vec![
                PathBuf::from("/System"),
                PathBuf::from("/Users/me/Documents"),
            ],
            vec![
                PathBuf::from("/Users/me/Library/Caches"),
                PathBuf::from("/Library/Caches"),
            ],
        )
    }

    #[test]
    fn allows_path_inside_approved_root() {
        let g = guard();
        let p = PathBuf::from("/Users/me/Library/Caches/com.example.app/blob");
        assert!(g.validate(&p).is_ok());
    }

    #[test]
    fn refuses_system_paths() {
        let g = guard();
        let p = PathBuf::from("/System/Library/Caches/whatever");
        assert_eq!(
            g.validate(&p),
            Err(RefusalReason::DeniedRoot(PathBuf::from("/System")))
        );
    }

    #[test]
    fn refuses_paths_outside_approved_roots() {
        let g = guard();
        let p = PathBuf::from("/Users/me/Projects/secret.txt");
        assert_eq!(g.validate(&p), Err(RefusalReason::OutsideApprovedRoots));
    }

    #[test]
    fn refuses_denied_even_if_it_looks_approved() {
        // Documents is deny-listed; even a nested path is refused.
        let g = guard();
        let p = PathBuf::from("/Users/me/Documents/taxes/2025.pdf");
        assert_eq!(
            g.validate(&p),
            Err(RefusalReason::DeniedRoot(PathBuf::from(
                "/Users/me/Documents"
            )))
        );
    }

    #[test]
    fn refuses_empty_path() {
        let g = guard();
        assert_eq!(g.validate(Path::new("")), Err(RefusalReason::Malformed));
    }

    #[test]
    fn parent_dir_traversal_cannot_escape_approved_root() {
        // A rule trying to escape via `..` normalizes to /Users/me which is not
        // an approved root, so it is refused.
        let g = guard();
        let p = PathBuf::from("/Users/me/Library/Caches/../../evil");
        assert_eq!(g.validate(&p), Err(RefusalReason::OutsideApprovedRoots));
    }

    #[test]
    fn approved_root_matches_through_a_symlinked_prefix() {
        // On macOS a temp dir lives under /var, itself a symlink to /private/var,
        // so a real file there canonicalizes to a /private/var/... path. The
        // guard must still recognize it as inside the approved root even though
        // the root was supplied via the /var name — regression for a clean that
        // refused everything under a symlinked prefix.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Caches");
        let file = root.join("com.example/blob.bin");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();

        let mut g = SafetyGuard::with_roots(vec![], vec![]);
        g.approve_root(root.clone());
        assert!(
            g.validate(&file).is_ok(),
            "a file inside an approved (possibly symlinked) root must be allowed"
        );
    }

    #[test]
    fn is_within_matches_exact_and_nested() {
        assert!(is_within(Path::new("/a/b"), Path::new("/a/b")));
        assert!(is_within(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a"), Path::new("/a/b")));
    }
}
