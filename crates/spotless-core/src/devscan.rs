//! Developer-junk scanner — Spotless's headline differentiator.
//!
//! Unlike fixed-path caches, build and dependency directories are scattered
//! throughout a developer's project folders. This module walks a root directory
//! and finds recognized artifact directories *by name*, validating ambiguous
//! ones against a marker file so a random folder called `target` is never
//! mistaken for a Rust build directory.
//!
//! Safety properties:
//! - A matched directory is recorded and **not descended into**, so nested
//!   artifacts (e.g. a `node_modules` inside another) are never double-counted.
//! - Matching is name-exact; the set of removable names is small and explicit
//!   (see [`is_artifact_dir_name`]), and every one is regenerable.
//! - This module never mutates the filesystem.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::SafetyTier;
use crate::scanner;

/// Where a marker file that validates an ambiguous match must live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// A file that must exist alongside the matched directory (same parent).
    Sibling(&'static str),
    /// A file that must exist inside the matched directory.
    Inside(&'static str),
}

/// A recognized developer-artifact directory pattern.
#[derive(Debug, Clone, Copy)]
pub struct DevPattern {
    /// The toolchain this belongs to, for UI grouping.
    pub tool: &'static str,
    /// Exact directory name to match.
    pub dir_name: &'static str,
    /// Optional validation marker for otherwise-ambiguous names.
    marker: Option<Marker>,
    pub safety: SafetyTier,
    pub description: &'static str,
}

/// The full set of recognized patterns. Kept small and explicit on purpose.
pub fn patterns() -> &'static [DevPattern] {
    &[
        DevPattern {
            tool: "Node.js",
            dir_name: "node_modules",
            marker: None,
            safety: SafetyTier::Caution,
            description: "Installed npm dependencies. Restore with a fresh install.",
        },
        DevPattern {
            tool: "Rust",
            dir_name: "target",
            marker: Some(Marker::Sibling("Cargo.toml")),
            safety: SafetyTier::Safe,
            description: "Cargo build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Python",
            dir_name: "__pycache__",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Compiled bytecode cache. Regenerated automatically.",
        },
        DevPattern {
            tool: "Python",
            dir_name: ".venv",
            marker: Some(Marker::Inside("pyvenv.cfg")),
            safety: SafetyTier::Caution,
            description: "Python virtual environment. Recreate from requirements.",
        },
        DevPattern {
            tool: "Python",
            dir_name: "venv",
            marker: Some(Marker::Inside("pyvenv.cfg")),
            safety: SafetyTier::Caution,
            description: "Python virtual environment. Recreate from requirements.",
        },
        DevPattern {
            tool: "Gradle",
            dir_name: ".gradle",
            marker: Some(Marker::Sibling("settings.gradle")),
            safety: SafetyTier::Safe,
            description: "Per-project Gradle cache. Rebuilt on next build.",
        },
        // The Kotlin DSL spells every Gradle file differently, so the same
        // directory needs one entry per marker it can be validated by. A
        // repeated `dir_name` is fine: `match_pattern` takes the first entry
        // whose marker actually holds.
        DevPattern {
            tool: "Gradle",
            dir_name: ".gradle",
            marker: Some(Marker::Sibling("settings.gradle.kts")),
            safety: SafetyTier::Safe,
            description: "Per-project Gradle cache. Rebuilt on next build.",
        },
        DevPattern {
            tool: "CocoaPods",
            dir_name: "Pods",
            marker: Some(Marker::Sibling("Podfile")),
            safety: SafetyTier::Caution,
            description: "Installed CocoaPods. Restore with `pod install`.",
        },
        DevPattern {
            tool: "Carthage",
            dir_name: "Carthage",
            marker: Some(Marker::Sibling("Cartfile")),
            safety: SafetyTier::Caution,
            description: "Carthage build output. Restore with `carthage bootstrap`.",
        },
        DevPattern {
            tool: "Next.js",
            dir_name: ".next",
            marker: Some(Marker::Sibling("package.json")),
            safety: SafetyTier::Safe,
            description: "Next.js build output. Rebuilt on next build.",
        },
        // --- Swift / Apple ---
        DevPattern {
            tool: "Swift Package Manager",
            dir_name: ".build",
            marker: Some(Marker::Sibling("Package.swift")),
            safety: SafetyTier::Safe,
            description: "SwiftPM build output and checkouts. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Xcode",
            dir_name: "DerivedData",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Xcode build intermediates and indexes. Rebuilt on next build.",
        },
        // --- JVM / Android ---
        DevPattern {
            tool: "Gradle",
            dir_name: "build",
            marker: Some(Marker::Sibling("build.gradle")),
            safety: SafetyTier::Safe,
            description: "Build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Gradle",
            dir_name: "build",
            marker: Some(Marker::Sibling("build.gradle.kts")),
            safety: SafetyTier::Safe,
            description: "Build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Maven",
            dir_name: "target",
            marker: Some(Marker::Sibling("pom.xml")),
            safety: SafetyTier::Safe,
            description: "Maven build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Android NDK",
            dir_name: ".cxx",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Native build intermediates. Rebuilt on next build.",
        },
        // --- C/C++ ---
        DevPattern {
            tool: "CMake",
            dir_name: "build",
            marker: Some(Marker::Sibling("CMakeLists.txt")),
            safety: SafetyTier::Safe,
            description: "CMake build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "CLion",
            dir_name: "cmake-build-debug",
            marker: None,
            safety: SafetyTier::Safe,
            description: "CLion debug build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "CLion",
            dir_name: "cmake-build-release",
            marker: None,
            safety: SafetyTier::Safe,
            description: "CLion release build output. Rebuilt on next build.",
        },
        // --- JavaScript / TypeScript ---
        DevPattern {
            tool: "Node.js",
            dir_name: "dist",
            marker: Some(Marker::Sibling("package.json")),
            safety: SafetyTier::Caution,
            description:
                "Bundled build output. Rebuilt on next build — unless the project commits it.",
        },
        DevPattern {
            tool: "Turborepo",
            dir_name: ".turbo",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Turborepo task cache. Rebuilt on next run.",
        },
        DevPattern {
            tool: "Parcel",
            dir_name: ".parcel-cache",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Parcel bundler cache. Rebuilt on next build.",
        },
        DevPattern {
            tool: "SvelteKit",
            dir_name: ".svelte-kit",
            marker: None,
            safety: SafetyTier::Safe,
            description: "SvelteKit build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Nuxt",
            dir_name: ".nuxt",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Nuxt build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Astro",
            dir_name: ".astro",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Astro build cache. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Angular",
            dir_name: ".angular",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Angular build cache. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Docusaurus",
            dir_name: ".docusaurus",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Docusaurus build cache. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Expo",
            dir_name: ".expo",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Expo local build cache. Rebuilt on next start.",
        },
        DevPattern {
            tool: "Istanbul",
            dir_name: ".nyc_output",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Coverage run output. Regenerated on the next test run.",
        },
        // --- Python ---
        DevPattern {
            tool: "pytest",
            dir_name: ".pytest_cache",
            marker: None,
            safety: SafetyTier::Safe,
            description: "pytest run cache. Regenerated on the next test run.",
        },
        DevPattern {
            tool: "mypy",
            dir_name: ".mypy_cache",
            marker: None,
            safety: SafetyTier::Safe,
            description: "mypy type-check cache. Regenerated on the next run.",
        },
        DevPattern {
            tool: "Ruff",
            dir_name: ".ruff_cache",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Ruff lint cache. Regenerated on the next run.",
        },
        DevPattern {
            tool: "tox",
            dir_name: ".tox",
            marker: None,
            safety: SafetyTier::Caution,
            description: "tox test environments. Recreated on the next `tox` run.",
        },
        DevPattern {
            tool: "Jupyter",
            dir_name: ".ipynb_checkpoints",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Notebook autosave checkpoints.",
        },
        // --- Go / PHP: `vendor` means a vendored dependency tree, but only
        // next to the manifest that put it there. ---
        DevPattern {
            tool: "Go",
            dir_name: "vendor",
            marker: Some(Marker::Sibling("go.mod")),
            safety: SafetyTier::Caution,
            description: "Vendored Go dependencies. Restore with `go mod vendor`.",
        },
        DevPattern {
            tool: "Composer",
            dir_name: "vendor",
            marker: Some(Marker::Sibling("composer.json")),
            safety: SafetyTier::Caution,
            description: "Installed PHP dependencies. Restore with `composer install`.",
        },
        // --- Elixir / Haskell / Zig / Terraform ---
        DevPattern {
            tool: "Elixir",
            dir_name: "_build",
            marker: Some(Marker::Sibling("mix.exs")),
            safety: SafetyTier::Safe,
            description: "Mix build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Elixir",
            dir_name: "deps",
            marker: Some(Marker::Sibling("mix.exs")),
            safety: SafetyTier::Caution,
            description: "Fetched Mix dependencies. Restore with `mix deps.get`.",
        },
        DevPattern {
            tool: "Stack",
            dir_name: ".stack-work",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Stack build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Zig",
            dir_name: ".zig-cache",
            marker: Some(Marker::Sibling("build.zig")),
            safety: SafetyTier::Safe,
            description: "Zig build cache. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Zig",
            dir_name: "zig-out",
            marker: Some(Marker::Sibling("build.zig")),
            safety: SafetyTier::Safe,
            description: "Zig build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Terraform",
            dir_name: ".terraform",
            marker: None,
            safety: SafetyTier::Caution,
            description: "Downloaded providers and modules. Restore with `terraform init`.",
        },
        // --- Flutter / Dart ---
        DevPattern {
            tool: "Flutter",
            dir_name: "build",
            marker: Some(Marker::Sibling("pubspec.yaml")),
            safety: SafetyTier::Safe,
            description: "Flutter build output. Rebuilt on next build.",
        },
        DevPattern {
            tool: "Dart",
            dir_name: ".dart_tool",
            marker: None,
            safety: SafetyTier::Safe,
            description: "Dart package config and build cache. Regenerated on `pub get`.",
        },
    ]
}

/// Whether `name` is an exact recognized artifact directory name.
///
/// A name alone is a weak check for the ambiguous patterns — `build`, `dist`,
/// `vendor` and `target` are ordinary folder names outside a project — so the
/// app layer gates cleaning on [`is_artifact_path`] instead, which re-runs the
/// marker validation. This stays as the cheap name-only test.
pub fn is_artifact_dir_name(name: &str) -> bool {
    patterns().iter().any(|p| p.dir_name == name)
}

/// Whether `path` is a recognized artifact directory *right now*: the name
/// matches a pattern and that pattern's marker still holds on disk.
///
/// This is what gates developer cleaning. Checking the marker again at clean
/// time (rather than trusting the scan that produced the path) means a request
/// to delete some unrelated `~/Documents/build` is refused, because there is no
/// `build.gradle` beside it — the same reason the scan would never have
/// reported it.
pub fn is_artifact_path(path: &Path) -> bool {
    match_pattern(path).is_some()
}

/// A discovered developer artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevArtifact {
    pub tool: String,
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub modified_secs: Option<u64>,
    pub safety: SafetyTier,
    pub description: String,
}

/// Options bounding the developer scan.
#[derive(Debug, Clone, Copy)]
pub struct DevScanOptions {
    /// Maximum directory depth to descend while searching (root is depth 0).
    pub max_depth: usize,
}

impl Default for DevScanOptions {
    fn default() -> Self {
        Self { max_depth: 7 }
    }
}

fn marker_ok(dir: &Path, marker: Option<Marker>) -> bool {
    match marker {
        None => true,
        Some(Marker::Sibling(file)) => dir.parent().map(|p| p.join(file).exists()).unwrap_or(false),
        Some(Marker::Inside(file)) => dir.join(file).exists(),
    }
}

/// Find the pattern (if any) that `dir` matches, honoring marker validation.
fn match_pattern(dir: &Path) -> Option<&'static DevPattern> {
    let name = dir.file_name()?.to_str()?;
    patterns()
        .iter()
        .find(|p| p.dir_name == name && marker_ok(dir, p.marker))
}

/// Whether `dir` is the iCloud container — `~/Library/Mobile Documents`.
///
/// Everything under it is served by a file provider that materialises content
/// on demand, so a `read_dir` there can block for as long as the network takes.
/// That is bad enough on its own — a scan that stops dead for half a minute in
/// a folder holding no build output — but the real damage is to cancelling:
/// a flag cannot interrupt a syscall that is waiting on iCloud, so pressing
/// Cancel does nothing at all until the fetch gives up.
///
/// It costs the scan a folder that *can* hold projects: iCloud Drive is
/// `Mobile Documents/com~apple~CloudDocs`. That is the deliberate trade —
/// the folders under it are, overwhelmingly, per-app iCloud containers with no
/// build output in them, and the one escape hatch is exact: only descent is
/// governed here, so choosing a folder inside iCloud as the scan root scans it.
///
/// Matched on the last two components rather than an absolute path, so it holds
/// for any home directory without having to resolve one.
fn is_icloud_container(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == "Mobile Documents")
        && dir
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "Library")
}

fn to_unix_secs(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Live counters for a running developer scan.
///
/// There is no total to divide by — how many project folders exist is only
/// known once the walk has been through them — so the UI gets counters and the
/// folder currently being looked at rather than a percentage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevProgress {
    /// Directories descended into so far.
    pub dirs_scanned: u64,
    /// Artifacts found so far.
    pub found: u64,
    /// Bytes across those artifacts.
    pub bytes_found: u64,
    /// The folder being walked when this tick was emitted, for the UI to show.
    pub current: String,
}

/// Scan `root` for developer artifacts, largest first.
pub fn scan_dev_artifacts(root: &Path, opts: DevScanOptions) -> Vec<DevArtifact> {
    scan_dev_artifacts_with(root, opts, &|_| {}, &|_| {}, &|| false)
}

/// How often a progress tick is emitted, in directories descended.
///
/// Every directory would be one IPC message per folder on disk; a project tree
/// is tens of thousands of them. Every 64 keeps the counters visibly moving
/// while the traffic stays in the hundreds.
const PROGRESS_EVERY: u64 = 64;

/// Scan `root`, handing each artifact to `on_artifact` as it is found and
/// ticking `on_progress` as the walk moves, stopping when `cancel` says so.
///
/// This is the form the UI drives. The artifacts also come back as the return
/// value, largest first — the streamed ones arrive in whatever order the walk
/// meets them, which is neither useful nor stable, so the caller has something
/// to settle the list with when the walk ends.
///
/// A cancelled walk returns what it had found; those are real artifacts,
/// correctly measured, so unlike a half-summed directory they are still worth
/// showing.
pub fn scan_dev_artifacts_with(
    root: &Path,
    opts: DevScanOptions,
    on_artifact: &dyn Fn(&DevArtifact),
    on_progress: &dyn Fn(DevProgress),
    cancel: &dyn Fn() -> bool,
) -> Vec<DevArtifact> {
    let mut walk = DevWalk {
        opts,
        on_artifact,
        on_progress,
        cancel,
        dirs: 0,
        bytes: 0,
        out: Vec::new(),
    };
    walk.descend(root, 0);
    // Final tick, so the counters the user is left looking at match the list.
    walk.tick(root);
    let mut out = walk.out;
    out.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
    out
}

/// One developer scan in progress: the callbacks, the counters, and what it
/// has found. Held together rather than passed as six arguments down a
/// recursion that is already deep.
struct DevWalk<'a> {
    opts: DevScanOptions,
    on_artifact: &'a dyn Fn(&DevArtifact),
    on_progress: &'a dyn Fn(DevProgress),
    cancel: &'a dyn Fn() -> bool,
    dirs: u64,
    bytes: u64,
    out: Vec<DevArtifact>,
}

impl DevWalk<'_> {
    fn tick(&self, current: &Path) {
        (self.on_progress)(DevProgress {
            dirs_scanned: self.dirs,
            found: self.out.len() as u64,
            bytes_found: self.bytes,
            current: current.to_string_lossy().into_owned(),
        });
    }

    fn descend(&mut self, dir: &Path, depth: usize) {
        if (self.cancel)() {
            return;
        }

        // Does this directory itself match an artifact pattern? If so, record it
        // and stop — never descend into a matched artifact.
        if depth > 0 {
            if let Some(pat) = match_pattern(dir) {
                let mut warnings = Vec::new();
                // Sizing a `node_modules` is itself a walk of tens of thousands
                // of files, so the cancel has to reach in here too — otherwise
                // stopping the scan waits out whichever artifact it was
                // measuring.
                let size = scanner::dir_size_with(dir, &mut warnings, self.cancel);
                if (self.cancel)() {
                    return; // Partial size: not worth reporting.
                }
                let modified_secs = dir
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(to_unix_secs);
                let artifact = DevArtifact {
                    tool: pat.tool.to_string(),
                    name: pat.dir_name.to_string(),
                    path: dir.to_path_buf(),
                    size_bytes: size,
                    modified_secs,
                    safety: pat.safety,
                    description: pat.description.to_string(),
                };
                self.bytes += size;
                (self.on_artifact)(&artifact);
                self.out.push(artifact);
                // Out of turn, so a find shows up beside the row it produced
                // rather than at the next multiple of 64 directories.
                self.tick(dir);
                return;
            }
        }

        if depth >= self.opts.max_depth {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        self.dirs += 1;
        if self.dirs.is_multiple_of(PROGRESS_EVERY) {
            self.tick(dir);
        }
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            // Only recurse into real directories (not symlinks, to avoid cycles).
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if is_icloud_container(&path) {
                    continue;
                }
                self.descend(&path, depth + 1);
                if (self.cancel)() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn write_file(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(path)
            .unwrap()
            .write_all(&vec![b'x'; bytes])
            .unwrap();
    }

    #[test]
    fn finds_node_modules_and_does_not_descend() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        write_file(&proj.join("node_modules/dep/index.js"), 100);
        // A nested node_modules inside the first must NOT be reported separately.
        write_file(&proj.join("node_modules/dep/node_modules/sub/x.js"), 50);

        let found = scan_dev_artifacts(dir.path(), DevScanOptions::default());
        let nm: Vec<_> = found.iter().filter(|a| a.name == "node_modules").collect();
        assert_eq!(nm.len(), 1);
        assert_eq!(nm[0].size_bytes, 150); // includes the nested contents
        assert_eq!(nm[0].tool, "Node.js");
    }

    #[test]
    fn target_requires_cargo_toml_sibling() {
        let dir = tempfile::tempdir().unwrap();
        // A `target` with a sibling Cargo.toml -> matched.
        write_file(&dir.path().join("rustproj/Cargo.toml"), 10);
        write_file(&dir.path().join("rustproj/target/debug/app"), 200);
        // A `target` with no Cargo.toml sibling -> ignored.
        write_file(&dir.path().join("random/target/data.bin"), 999);

        let found = scan_dev_artifacts(dir.path(), DevScanOptions::default());
        let targets: Vec<_> = found.iter().filter(|a| a.name == "target").collect();
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("rustproj/target"));
        assert_eq!(targets[0].size_bytes, 200);
    }

    #[test]
    fn venv_requires_inside_marker() {
        let dir = tempfile::tempdir().unwrap();
        // A real venv contains pyvenv.cfg.
        write_file(&dir.path().join("a/.venv/pyvenv.cfg"), 5);
        write_file(&dir.path().join("a/.venv/lib/pkg.py"), 300);
        // A folder named venv without the marker is ignored.
        write_file(&dir.path().join("b/venv/notes.txt"), 40);

        let found = scan_dev_artifacts(dir.path(), DevScanOptions::default());
        let venvs: Vec<_> = found
            .iter()
            .filter(|a| a.name == ".venv" || a.name == "venv")
            .collect();
        assert_eq!(venvs.len(), 1);
        assert_eq!(venvs[0].name, ".venv");
    }

    #[test]
    fn streams_each_artifact_as_it_is_found() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("proj-a/node_modules/x.js"), 100);
        write_file(&dir.path().join("proj-b/node_modules/y.js"), 50);

        let streamed = std::cell::RefCell::new(Vec::new());
        let ticks = std::cell::Cell::new(0);
        let found = scan_dev_artifacts_with(
            dir.path(),
            DevScanOptions::default(),
            &|a| streamed.borrow_mut().push(a.path.clone()),
            &|_| ticks.set(ticks.get() + 1),
            &|| false,
        );

        assert_eq!(
            streamed.borrow().len(),
            2,
            "both artifacts must be streamed"
        );
        assert_eq!(found.len(), 2);
        // The return value is settled largest-first; the stream is not.
        assert_eq!(found[0].size_bytes, 100);
        assert!(ticks.get() > 0, "progress must be reported");
    }

    #[test]
    fn cancelling_stops_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            write_file(&dir.path().join(format!("proj-{i}/node_modules/x.js")), 10);
        }

        let seen = std::cell::Cell::new(0);
        let found = scan_dev_artifacts_with(
            dir.path(),
            DevScanOptions::default(),
            &|_| seen.set(seen.get() + 1),
            &|_| {},
            // Stop as soon as the first artifact has been handed over.
            &|| seen.get() >= 1,
        );

        assert_eq!(seen.get(), 1);
        // What it did find is kept: those are whole artifacts, correctly sized.
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn the_icloud_container_is_not_descended_into() {
        let dir = tempfile::tempdir().unwrap();
        // The shape on a real machine: the container sits under `Library`.
        write_file(
            &dir.path()
                .join("Library/Mobile Documents/proj/node_modules/x.js"),
            100,
        );
        // A folder of the same name somewhere else is not the container.
        write_file(
            &dir.path().join("Mobile Documents/proj/node_modules/y.js"),
            50,
        );

        let found = scan_dev_artifacts(dir.path(), DevScanOptions::default());
        let paths: Vec<_> = found.iter().map(|a| a.path.clone()).collect();
        assert_eq!(paths.len(), 1, "found {paths:?}");
        assert!(paths[0].ends_with("Mobile Documents/proj/node_modules"));
        assert!(!paths[0].starts_with(dir.path().join("Library")));
    }

    #[test]
    fn icloud_chosen_as_the_root_is_still_scanned() {
        // Only descent is governed: picking the folder by hand is asking for it.
        let dir = tempfile::tempdir().unwrap();
        let icloud = dir.path().join("Library/Mobile Documents");
        write_file(&icloud.join("proj/node_modules/x.js"), 100);

        let found = scan_dev_artifacts(&icloud, DevScanOptions::default());
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn respects_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        // node_modules buried deep.
        write_file(&dir.path().join("a/b/c/d/node_modules/x.js"), 10);

        let shallow = scan_dev_artifacts(dir.path(), DevScanOptions { max_depth: 2 });
        assert!(shallow.is_empty());

        let deep = scan_dev_artifacts(dir.path(), DevScanOptions { max_depth: 8 });
        assert_eq!(deep.len(), 1);
    }

    #[test]
    fn recognized_names_gate() {
        assert!(is_artifact_dir_name("node_modules"));
        assert!(is_artifact_dir_name("target"));
        assert!(!is_artifact_dir_name("src"));
        assert!(!is_artifact_dir_name("Documents"));
    }

    #[test]
    fn artifact_path_gate_rechecks_the_marker() {
        // The name gate alone would accept any folder called `build` or
        // `vendor`; the path gate is what keeps a plain folder of the same name
        // out of the cleaner.
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("gradleproj/build.gradle"), 10);
        write_file(&dir.path().join("gradleproj/build/classes/A.class"), 20);
        write_file(&dir.path().join("notes/build/plan.md"), 30);

        assert!(is_artifact_dir_name("build")); // name alone: both would pass
        assert!(is_artifact_path(&dir.path().join("gradleproj/build")));
        assert!(!is_artifact_path(&dir.path().join("notes/build")));
        // Unmarked patterns still resolve by name.
        assert!(is_artifact_path(&dir.path().join("anywhere/node_modules")));
    }

    #[test]
    fn same_name_patterns_match_by_their_own_marker() {
        // `build` belongs to four toolchains and `target` to two. Each entry is
        // validated by its own marker, so the first one whose marker holds wins
        // and the tool label stays truthful.
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("flutterapp/pubspec.yaml"), 10);
        write_file(&dir.path().join("flutterapp/build/app.dill"), 40);
        write_file(&dir.path().join("cmakeproj/CMakeLists.txt"), 10);
        write_file(&dir.path().join("cmakeproj/build/CMakeCache.txt"), 60);
        write_file(&dir.path().join("javaproj/pom.xml"), 10);
        write_file(&dir.path().join("javaproj/target/app.jar"), 80);

        let found = scan_dev_artifacts(dir.path(), DevScanOptions::default());
        let tool_of = |name: &str| {
            found
                .iter()
                .find(|a| a.path.ends_with(name))
                .map(|a| a.tool.as_str())
        };
        assert_eq!(tool_of("flutterapp/build"), Some("Flutter"));
        assert_eq!(tool_of("cmakeproj/build"), Some("CMake"));
        assert_eq!(tool_of("javaproj/target"), Some("Maven"));
    }

    #[test]
    fn kotlin_dsl_gradle_projects_are_recognized() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("kproj/settings.gradle.kts"), 10);
        write_file(&dir.path().join("kproj/.gradle/8.7/x.bin"), 50);

        let found = scan_dev_artifacts(dir.path(), DevScanOptions::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, ".gradle");
        assert_eq!(found[0].tool, "Gradle");
    }
}
