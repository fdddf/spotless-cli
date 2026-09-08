//! Leftovers of applications that are no longer installed.
//!
//! [`apps`](crate::apps) works forwards: given an installed app, find the
//! support files named after it. This module works backwards: list the
//! directories where support files live, and keep the entries that no installed
//! app can account for. What is left is the residue of apps the user dragged to
//! the Trash without uninstalling.
//!
//! # Why this only ever matches bundle identifiers
//!
//! The forward scan can safely match on an app's *display name*, because the
//! name comes from a bundle that exists — "Foo" is only ever compared against
//! entries while uninstalling Foo. Backwards, there is no such anchor, and a
//! name-shaped entry is overwhelmingly not an app:
//!
//! - `Application Support/Google` is a *vendor* directory shared by Chrome,
//!   Drive and the updater; no single app owns it.
//! - `Application Support/Code` belongs to Visual Studio Code, whose bundle is
//!   named "Visual Studio Code" — the names simply do not correspond.
//! - `Application Support/pypoetry`, `Caches/typescript`, `Caches/carthage` are
//!   command-line tools that never had a bundle at all.
//! - `Caches/GeoServices`, `HTTPStorages/askpermissiond` are Apple daemons whose
//!   directory names carry no `com.apple.` marker.
//!
//! A survey of one developer's `~/Library` found 317 such entries totalling
//! 8.2 GB and *not one* of them was a genuine orphan, while the identifier-shaped
//! entries were almost all real. Since the cost of a false positive here is
//! deleting live user data, name matching is not merely disabled — it is absent.
//!
//! This module never mutates the filesystem.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::apps;
use crate::scanner;

/// One support file left behind by an app that is no longer installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanItem {
    /// Human-readable category, matching the forward scan's vocabulary.
    pub kind: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// The leftovers of one vanished app, grouped by the identifier they share.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanApp {
    /// The bundle identifier the entries are named after, e.g. `com.acme.foo`.
    pub bundle_id: String,
    /// A readable name derived from the identifier, for the list row.
    pub display_name: String,
    pub items: Vec<OrphanItem>,
    pub total_bytes: u64,
}

/// The `~/Library` directories scanned for orphans, as `(category, subdirectory)`.
///
/// A subset of the forward scan's rules: only the directories whose entries are
/// *named by identifier*. `Logs/DiagnosticReports` and
/// `Application Support/CrashReporter` are deliberately absent — they are keyed
/// by display name, which this module cannot match on.
const SCAN_DIRS: &[(&str, &str)] = &[
    ("Application Support", "Application Support"),
    ("Caches", "Caches"),
    ("Preferences", "Preferences"),
    ("Containers", "Containers"),
    ("Group Containers", "Group Containers"),
    ("Saved State", "Saved Application State"),
    ("HTTP Storage", "HTTPStorages"),
    ("WebKit", "WebKit"),
    ("Cookies", "Cookies"),
    ("App Scripts", "Application Scripts"),
    ("Launch Agent", "LaunchAgents"),
    ("Logs", "Logs"),
];

/// Entries that are part of the *structure* of a scanned directory rather than
/// any app's leftovers, as `(subdirectory, entry name)`.
///
/// The forward scan cannot nominate these — it only ever looks for entries
/// matching a known identifier — but a backwards scan lists everything, so they
/// have to be named. `Preferences/ByHost` is the one that matters: it is itself
/// a scanned directory, and removing it would take every per-host preference on
/// the machine with it.
const STRUCTURAL_ENTRIES: &[(&str, &str)] = &[
    ("Preferences", "ByHost"),
    ("WebKit", "Databases"),
    ("WebKit", "MediaKeys"),
    ("WebKit", "NetworkProcess"),
    ("Caches", "com.apple.nsurlsessiond"),
    ("Application Support", "CrashReporter"),
    ("Application Support", "com.apple.sharedfilelist"),
];

/// Suffixes that decorate an identifier-named file, stripped before the name is
/// tested for identifier shape.
const DECORATIONS: &[&str] = &[
    ".plist",
    ".savedState",
    ".binarycookies",
    ".ShipIt",
    ".sfl2",
    ".sfl3",
];

/// Whether `token` has the shape of a bundle identifier: at least three
/// dot-separated segments, each non-empty and made of characters an identifier
/// may contain.
///
/// Three rather than two is a deliberate tightening. Two-segment names
/// (`fast-unarchiver`, `default.store`) are far more often a tool or a stray
/// file than an app; the reverse-DNS identifiers this looks for
/// (`com.acme.foo`) always have at least three.
pub fn looks_like_bundle_id(token: &str) -> bool {
    let segments: Vec<&str> = token.split('.').collect();
    if segments.len() < 3 {
        return false;
    }
    segments.iter().all(|s| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// Strip the team-identifier or `group.` prefix a group container carries, so
/// `UBF8T346G9.com.microsoft.teams` is tested as `com.microsoft.teams`.
///
/// Returns the original token when there is nothing to strip.
fn strip_group_prefix(token: &str) -> &str {
    if let Some(rest) = token.strip_prefix("group.") {
        return rest;
    }
    // A team identifier is exactly ten upper-case alphanumerics.
    if let Some((head, rest)) = token.split_once('.') {
        if head.len() == 10
            && head
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return rest.strip_prefix("groups.").unwrap_or(rest);
        }
    }
    token
}

/// Whether `token` names something belonging to macOS itself.
///
/// A plain `com.apple.` prefix test is not enough: Apple's own group containers
/// arrive as `243LU875E5.groups.com.apple.podcasts`, and a few system groups use
/// neither form (`group.is.workflow.shortcuts` is Shortcuts). The marker is
/// therefore looked for anywhere in the token.
fn is_apple_identifier(token: &str) -> bool {
    let t = token.to_lowercase();
    t.contains("com.apple.") || t.starts_with("apple.") || t.contains("is.workflow.")
}

/// Every bundle identifier that some installed app accounts for.
///
/// Includes identifiers nested *inside* installed bundles — login items, XPC
/// services, frameworks, plug-ins. Those helpers write their own support files
/// under their own identifier while having no `.app` of their own, so without
/// them a live helper like `com.microsoft.autoupdate.fba` reads as an orphan.
/// Spotlight does not index nested bundles, so they have to be read directly.
///
/// The scan is bounded to the handful of directories a helper can live in,
/// one level deep. Walking each bundle in full finds perhaps 60% more
/// identifiers for two orders of magnitude more I/O.
pub fn installed_bundle_ids(app_dirs: &[PathBuf]) -> HashSet<String> {
    installed_app_bundles(app_dirs)
        .par_iter()
        .flat_map_iter(|app| bundle_ids_within(app))
        .collect()
}

/// The directories inside an `.app` where a nested bundle may sit.
///
/// `Contents/Library/LaunchAgents` earns its place: Zoom ships its updater
/// there as `ZoomUpdater.app` with identifier `us.zoom.updater`, and without
/// this entry the updater's support files are reported as orphans of an app
/// that is plainly still installed.
const NESTED_BUNDLE_DIRS: &[&str] = &[
    "Contents/Library/LoginItems",
    "Contents/Library/LaunchAgents",
    "Contents/Library/LaunchDaemons",
    "Contents/Library/QuickLook",
    "Contents/Library/Spotlight",
    "Contents/Library/SystemExtensions",
    "Contents/Library/Automator",
    "Contents/Library/Services",
    "Contents/XPCServices",
    "Contents/PlugIns",
    "Contents/Extensions",
    "Contents/Frameworks",
    "Contents/Helpers",
    "Contents/Resources",
    "Contents/MacOS",
];

/// The identifier of `app` plus those of the bundles nested one level inside it.
fn bundle_ids_within(app: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(id) = read_bundle_id(&app.join("Contents/Info.plist")) {
        ids.push(id);
    }
    for sub in NESTED_BUNDLE_DIRS {
        let Ok(entries) = std::fs::read_dir(app.join(sub)) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            // Apps and appexes keep their plist under Contents/; frameworks and
            // some bundles keep it under Resources/ or Versions/Current/.
            let candidates = [
                child.join("Contents/Info.plist"),
                child.join("Resources/Info.plist"),
                child.join("Versions/Current/Resources/Info.plist"),
            ];
            for plist in candidates {
                if let Some(id) = read_bundle_id(&plist) {
                    ids.push(id);
                    break;
                }
            }
        }
    }
    ids
}

fn read_bundle_id(plist: &Path) -> Option<String> {
    let value = plist::Value::from_file(plist).ok()?;
    let id = value
        .as_dictionary()?
        .get("CFBundleIdentifier")?
        .as_string()?;
    Some(id.to_lowercase())
}

/// Every installed bundle found under `app_dirs`.
///
/// A bundle is any directory holding `Contents/Info.plist` — not just `.app`.
/// Input methods (`.app`), preference panes (`.prefPane`) and plug-ins
/// (`.bundle`) all own support files under their own identifier, so restricting
/// this to `.app` would report a live component as an orphan.
///
/// Directories that are not themselves bundles are descended into one level,
/// which is how a suite installs itself (`/Applications/Adobe Photoshop 2024/
/// Adobe Photoshop 2024.app`) and how vendors lay out
/// `/Library/Application Support/<vendor>/<helper>.app`.
fn installed_app_bundles(app_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let is_bundle = |p: &Path| p.join("Contents/Info.plist").is_file();
    let mut apps = Vec::new();
    for dir in app_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if is_bundle(&path) {
                apps.push(path);
                continue;
            }
            let Ok(nested) = std::fs::read_dir(&path) else {
                continue;
            };
            apps.extend(
                nested
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && is_bundle(p)),
            );
        }
    }
    apps
}

/// The directories searched when deciding what is installed.
///
/// Much wider than [`apps::default_app_dirs`], which drives the uninstaller
/// list, and deliberately so. The uninstaller only needs the apps a user can
/// remove; this needs *everything that might own a support file*, because an
/// omission here does not merely hide a row — it offers a live component's data
/// up for deletion. An app under `/System/Applications` cannot be uninstalled,
/// but its files must still never be called orphaned.
///
/// `/Library/Input Methods` is the case that proves the point: Sogou and WeType
/// install there and nowhere else, and are reported as orphans without it.
pub fn installed_app_dirs() -> Vec<PathBuf> {
    let mut dirs = apps::default_app_dirs();
    for path in [
        "/Applications/Utilities",
        "/System/Applications",
        "/System/Applications/Utilities",
        "/System/Library/CoreServices",
        "/Library/Input Methods",
        "/Library/PreferencePanes",
        "/Library/Internet Plug-Ins",
        "/Library/CoreServices",
        "/Library/QuickLook",
        "/Library/Spotlight",
        "/Library/Application Support",
        "/Library/PrivilegedHelperTools",
    ] {
        dirs.push(PathBuf::from(path));
    }
    if let Some(home) = crate::paths::home_dir() {
        for sub in [
            "Library/Input Methods",
            "Library/PreferencePanes",
            "Library/Internet Plug-Ins",
            "Library/Services",
        ] {
            dirs.push(home.join(sub));
        }
    }
    dirs
}

/// Whether `token` is accounted for by one of `installed`.
///
/// A match may be exact or a boundary-respecting extension of an installed
/// identifier, so that `com.acme.foo.helper` and `com.acme.foo.ShipIt` are
/// recognised as belonging to an installed `com.acme.foo` — while
/// `com.acme.foobar`, a different app, is not.
fn is_accounted_for(token: &str, installed: &HashSet<String>) -> bool {
    let t = token.to_lowercase();
    if installed.contains(&t) {
        return true;
    }
    installed.iter().any(|known| {
        t.len() > known.len()
            && t.starts_with(known.as_str())
            && matches!(t.as_bytes()[known.len()], b'.' | b'_')
    })
}

/// Strip a decorating suffix from an entry name, leaving the identifier.
fn token_of(name: &str) -> &str {
    for suffix in DECORATIONS {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

/// Find the leftovers under `home` that no installed app accounts for, grouped
/// by identifier and sorted largest first.
///
/// `installed` is passed in rather than computed so the caller can build it once
/// and so tests can drive the filter directly.
pub fn find_orphans(home: &Path, installed: &HashSet<String>) -> Vec<OrphanApp> {
    let lib = home.join("Library");
    let mut candidates: Vec<(String, OrphanItem)> = Vec::new();
    let mut seen = HashSet::new();

    for (kind, sub) in SCAN_DIRS {
        let dir = lib.join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if STRUCTURAL_ENTRIES
                .iter()
                .any(|(s, e)| s == sub && *e == name)
            {
                continue;
            }
            let token = token_of(name);
            if !looks_like_bundle_id(token) {
                continue;
            }
            let id = strip_group_prefix(token);
            if is_apple_identifier(id) || is_accounted_for(id, installed) {
                continue;
            }
            let path = entry.path();
            if !seen.insert(path.clone()) {
                continue;
            }
            candidates.push((
                id.to_lowercase(),
                OrphanItem {
                    kind: (*kind).to_string(),
                    path,
                    size_bytes: 0,
                },
            ));
        }
    }

    // Sizing walks whole subtrees, so it happens once, in parallel, after the
    // cheap filters have cut the candidate set down.
    let sizes: Vec<u64> = candidates
        .par_iter()
        .map(|(_, item)| {
            let mut warnings = Vec::new();
            if item.path.is_dir() {
                scanner::dir_size(&item.path, &mut warnings)
            } else {
                item.path.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .collect();

    let mut grouped: HashMap<String, OrphanApp> = HashMap::new();
    for ((id, mut item), size) in candidates.into_iter().zip(sizes) {
        item.size_bytes = size;
        let group = grouped.entry(id.clone()).or_insert_with(|| OrphanApp {
            display_name: display_name_for(&id),
            bundle_id: id,
            items: Vec::new(),
            total_bytes: 0,
        });
        group.total_bytes += size;
        group.items.push(item);
    }

    let mut out: Vec<OrphanApp> = grouped.into_values().collect();
    for group in &mut out {
        group.items.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    }
    out.sort_by(|a, b| {
        b.total_bytes
            .cmp(&a.total_bytes)
            .then_with(|| a.bundle_id.cmp(&b.bundle_id))
    });
    out
}

/// A readable name for the row: the last identifier segment, which is
/// conventionally the product name (`com.acme.ScreenRecorder` → "ScreenRecorder").
fn display_name_for(bundle_id: &str) -> String {
    bundle_id
        .rsplit('.')
        .find(|s| !s.is_empty())
        .unwrap_or(bundle_id)
        .to_string()
}

/// Whether `path` is something the orphan cleaner may remove: an entry directly
/// inside one of the [`SCAN_DIRS`], under this user's `~/Library`.
///
/// The app layer re-checks every path with this before removal, because the
/// paths arrive back from the frontend and are therefore untrusted. It is what
/// stops a scanned directory itself, or anything outside the table, from being
/// named — the same role [`apps::is_system_leftover_path`] plays for the
/// root-domain leftovers.
pub fn is_orphan_path(path: &Path, home: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lib = home.join("Library");
    SCAN_DIRS.iter().any(|(_, sub)| {
        parent == lib.join(sub)
            && !STRUCTURAL_ENTRIES
                .iter()
                .any(|(s, e)| s == sub && *e == name)
    })
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

    fn installed(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_identifier_shaped_names_are_candidates() {
        // The real false positives from the survey.
        assert!(!looks_like_bundle_id("Google"));
        assert!(!looks_like_bundle_id("Code"));
        assert!(!looks_like_bundle_id("Adobe Photoshop 2024 Settings"));
        assert!(!looks_like_bundle_id("pypoetry"));
        assert!(!looks_like_bundle_id("fast-unarchiver"));
        assert!(!looks_like_bundle_id("default.store"));
        assert!(!looks_like_bundle_id("C77D232F-C92D-45F9-A520-6438CBDB87E6"));

        assert!(looks_like_bundle_id("com.acme.foo"));
        assert!(looks_like_bundle_id("com.acme.foo.helper"));
        assert!(looks_like_bundle_id("net.yanue.V2rayU"));
        assert!(looks_like_bundle_id("ai.bloop.vibe-kanban"));
        // Malformed: an empty segment is not an identifier.
        assert!(!looks_like_bundle_id("com..foo"));
        assert!(!looks_like_bundle_id(".com.acme"));
    }

    #[test]
    fn group_container_prefixes_are_stripped_before_matching() {
        assert_eq!(strip_group_prefix("UBF8T346G9.com.microsoft.teams"), "com.microsoft.teams");
        assert_eq!(strip_group_prefix("group.uk.hudson.Kickstart"), "uk.hudson.Kickstart");
        assert_eq!(
            strip_group_prefix("243LU875E5.groups.com.apple.podcasts"),
            "com.apple.podcasts"
        );
        // A short leading segment is part of the identifier, not a team id.
        assert_eq!(strip_group_prefix("com.acme.foo"), "com.acme.foo");
        assert_eq!(strip_group_prefix("ai.bloop.vibe"), "ai.bloop.vibe");
    }

    #[test]
    fn apple_owned_identifiers_are_never_orphans() {
        assert!(is_apple_identifier("com.apple.podcasts"));
        // The forms a plain prefix test misses, both seen on a real machine.
        assert!(is_apple_identifier(strip_group_prefix(
            "243LU875E5.groups.com.apple.podcasts"
        )));
        assert!(is_apple_identifier("group.is.workflow.shortcuts"));
        assert!(!is_apple_identifier("com.acme.apple-sauce"));
    }

    #[test]
    fn helpers_of_an_installed_app_are_accounted_for() {
        let known = installed(&["com.microsoft.autoupdate", "com.acme.foo"]);
        assert!(is_accounted_for("com.microsoft.autoupdate.fba", &known));
        assert!(is_accounted_for("com.acme.foo.ShipIt", &known));
        assert!(is_accounted_for("COM.ACME.FOO", &known));
        // A different app that merely shares a prefix is still an orphan.
        assert!(!is_accounted_for("com.acme.foobar", &known));
        assert!(!is_accounted_for("com.acme.foo-bar", &known));
    }

    #[test]
    fn find_orphans_groups_by_identifier_and_skips_installed_apps() {
        let home = tempfile::tempdir().unwrap();
        let lib = home.path().join("Library");

        // One vanished app with leftovers in three places.
        write_file(&lib.join("Containers/com.acme.gone/Data/db"), 300);
        write_file(&lib.join("Preferences/com.acme.gone.plist"), 100);
        write_file(&lib.join("Caches/com.acme.gone/blob.bin"), 600);
        // An installed app, and one of its helpers.
        write_file(&lib.join("Caches/com.acme.here/blob.bin"), 999);
        write_file(&lib.join("Preferences/com.acme.here.fba.plist"), 999);
        // Apple's own.
        write_file(&lib.join("Caches/com.apple.Safari/x"), 999);
        write_file(&lib.join("Group Containers/243LU875E5.groups.com.apple.podcasts/x"), 999);
        // Name-shaped entries: vendor dirs, tools, an Apple daemon.
        write_file(&lib.join("Application Support/Google/Chrome/Default/data"), 999);
        write_file(&lib.join("Caches/typescript/x"), 999);
        write_file(&lib.join("HTTPStorages/askpermissiond/x"), 999);

        let found = find_orphans(home.path(), &installed(&["com.acme.here"]));

        assert_eq!(found.len(), 1, "found: {found:?}");
        let gone = &found[0];
        assert_eq!(gone.bundle_id, "com.acme.gone");
        assert_eq!(gone.display_name, "gone");
        assert_eq!(gone.items.len(), 3);
        assert_eq!(gone.total_bytes, 1000);
        // Largest first, so the expanded row leads with what matters.
        assert_eq!(gone.items[0].size_bytes, 600);
    }

    #[test]
    fn group_containers_are_attributed_to_the_app_not_the_team() {
        let home = tempfile::tempdir().unwrap();
        let lib = home.path().join("Library");
        write_file(&lib.join("Group Containers/UBF8T346G9.com.microsoft.teams/db"), 50);
        write_file(&lib.join("Caches/com.microsoft.teams/blob"), 70);

        let found = find_orphans(home.path(), &HashSet::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bundle_id, "com.microsoft.teams");
        assert_eq!(found[0].total_bytes, 120);
    }

    #[test]
    fn the_structural_directories_are_never_nominated() {
        let home = tempfile::tempdir().unwrap();
        let lib = home.path().join("Library");
        // ByHost is itself scanned; nominating it would take every per-host
        // preference on the machine.
        write_file(&lib.join("Preferences/ByHost/com.acme.gone.ABC.plist"), 10);
        write_file(&lib.join("WebKit/Databases/x"), 10);

        let found = find_orphans(home.path(), &HashSet::new());
        let paths: Vec<String> = found
            .iter()
            .flat_map(|a| a.items.iter().map(|i| i.path.display().to_string()))
            .collect();
        assert!(
            paths.iter().all(|p| !p.ends_with("ByHost") && !p.ends_with("Databases")),
            "nominated a structural directory: {paths:?}"
        );
    }

    #[test]
    fn removal_is_confined_to_entries_inside_a_scanned_directory() {
        let home = Path::new("/Users/me");
        assert!(is_orphan_path(
            Path::new("/Users/me/Library/Caches/com.acme.gone"),
            home
        ));
        assert!(is_orphan_path(
            Path::new("/Users/me/Library/Preferences/com.acme.gone.plist"),
            home
        ));
        // The scanned directories themselves, and anything above them.
        assert!(!is_orphan_path(Path::new("/Users/me/Library/Caches"), home));
        assert!(!is_orphan_path(Path::new("/Users/me/Library"), home));
        // The structural entries, restated here because this is the check that
        // actually runs before a delete.
        assert!(!is_orphan_path(
            Path::new("/Users/me/Library/Preferences/ByHost"),
            home
        ));
        // Deeper than a nominated entry: the top-level entry is what gets
        // removed, and its subtree goes with it.
        assert!(!is_orphan_path(
            Path::new("/Users/me/Library/Caches/com.acme.gone/inner"),
            home
        ));
        // Directories that are not scanned at all, and other users' homes.
        assert!(!is_orphan_path(
            Path::new("/Users/me/Library/Keychains/x"),
            home
        ));
        assert!(!is_orphan_path(
            Path::new("/Users/other/Library/Caches/com.acme.gone"),
            home
        ));
        assert!(!is_orphan_path(Path::new("/Library/Caches/com.acme.gone"), home));
    }

    #[test]
    fn nested_bundle_ids_are_read_from_an_installed_app() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Foo.app");
        let plist = |id: &str| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>{id}</string>
</dict></plist>"#
            )
        };
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(app.join("Contents/Info.plist"), plist("com.acme.foo")).unwrap();
        let helper = app.join("Contents/Library/LoginItems/Helper.app/Contents");
        fs::create_dir_all(&helper).unwrap();
        fs::write(helper.join("Info.plist"), plist("com.acme.foo.helper")).unwrap();
        let xpc = app.join("Contents/XPCServices/Svc.xpc/Contents");
        fs::create_dir_all(&xpc).unwrap();
        fs::write(xpc.join("Info.plist"), plist("com.acme.svc")).unwrap();

        let ids = installed_bundle_ids(&[dir.path().to_path_buf()]);
        assert!(ids.contains("com.acme.foo"));
        assert!(ids.contains("com.acme.foo.helper"));
        // A helper whose identifier is not an extension of its host's: without
        // reading it, its support files would read as orphaned.
        assert!(ids.contains("com.acme.svc"));
    }

    #[test]
    fn a_bundle_that_is_not_an_app_still_counts_as_installed() {
        // Input methods and preference panes own support files under their own
        // identifier while living outside /Applications entirely.
        let dir = tempfile::tempdir().unwrap();
        for (name, id) in [
            ("WeType.app", "com.tencent.inputmethod.wetype"),
            ("Logi.prefPane", "com.logi.ai.portal"),
        ] {
            let contents = dir.path().join(name).join("Contents");
            fs::create_dir_all(&contents).unwrap();
            fs::write(
                contents.join("Info.plist"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>{id}</string>
</dict></plist>"#
                ),
            )
            .unwrap();
        }

        let ids = installed_bundle_ids(&[dir.path().to_path_buf()]);
        assert!(ids.contains("com.tencent.inputmethod.wetype"));
        assert!(ids.contains("com.logi.ai.portal"));
    }

    #[test]
    fn apps_inside_a_suite_folder_count_as_installed() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("Adobe Photoshop 2024/Adobe Photoshop 2024.app/Contents");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.adobe.Photoshop</string>
</dict></plist>"#,
        )
        .unwrap();

        let ids = installed_bundle_ids(&[dir.path().to_path_buf()]);
        assert!(ids.contains("com.adobe.photoshop"));
    }
}
