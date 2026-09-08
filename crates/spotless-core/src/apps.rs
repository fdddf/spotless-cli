//! Application uninstaller.
//!
//! Lists installed `.app` bundles and, for a chosen app, finds the support
//! files it leaves scattered across `~/Library` so a full uninstall removes the
//! app *and* its leftovers (preferences, caches, containers, saved state,
//! launch agents, …).
//!
//! The leftover-location logic is split in two so it can be unit-tested: the
//! rules ([`leftover_rules`]) are a pure list of "look in this directory for
//! entries named after the app", and [`find_leftovers`] reads those directories,
//! keeps the entries that match, and sizes them. Matching by directory listing
//! rather than by exact path is what catches the helper-suffixed siblings
//! (`<id>.ShipIt`, `<id>.helper`, ByHost preferences, crash reports) that an
//! exact-path probe misses.
//!
//! This module never mutates the filesystem.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::scanner;

/// An installed application bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    /// Display name without the `.app` extension.
    pub name: String,
    /// Absolute path to the `.app` bundle.
    pub path: PathBuf,
    /// `CFBundleIdentifier` from the bundle's Info.plist, if readable.
    pub bundle_id: Option<String>,
    /// Total size of the bundle in bytes, or `None` if not measured yet.
    /// [`list_apps`] leaves this unset; [`app_sizes`] fills it in.
    pub size_bytes: Option<u64>,
}

/// A support file/directory left behind by an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leftover {
    /// Human-readable category (e.g. "Preferences", "Caches").
    pub kind: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Whether removing this needs an administrator password. True for the
    /// root-owned leftovers under `/Library`; the UI marks them so the prompt
    /// is not a surprise.
    pub requires_admin: bool,
}

/// The full uninstall plan for one app: the bundle plus its leftovers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallPlan {
    pub app: AppInfo,
    pub leftovers: Vec<Leftover>,
    /// Total reclaimable bytes (bundle + all leftovers).
    pub total_bytes: u64,
}

/// The default directories scanned for applications.
pub fn default_app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = crate::paths::home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// Read `CFBundleIdentifier` from an app bundle's Info.plist.
fn read_bundle_id(app_path: &Path) -> Option<String> {
    let info = app_path.join("Contents/Info.plist");
    let value = plist::Value::from_file(info).ok()?;
    value
        .as_dictionary()?
        .get("CFBundleIdentifier")?
        .as_string()
        .map(|s| s.to_string())
}

/// List `.app` bundles found directly inside each of `dirs`, without sizing
/// them. Sizing means walking every file in every bundle, which takes seconds
/// for a full Applications folder, so it is left to [`app_sizes`] and the
/// listing itself stays fast enough to render immediately.
pub fn list_apps(dirs: &[PathBuf]) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map(|e| e == "app").unwrap_or(false) {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                apps.push(AppInfo {
                    name,
                    bundle_id: read_bundle_id(&path),
                    size_bytes: None,
                    path,
                });
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Measure the on-disk size of each of `paths`, as (path, bytes) pairs.
///
/// The walks are independent and dominated by filesystem metadata calls, so
/// they run in parallel across the rayon pool.
pub fn app_sizes(paths: &[PathBuf]) -> Vec<(PathBuf, u64)> {
    paths
        .par_iter()
        .map(|path| {
            let mut warnings = Vec::new();
            (path.clone(), scanner::dir_size(path, &mut warnings))
        })
        .collect()
}

/// How an entry name in a scanned directory is compared against a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// The name starts with the token, at a separator boundary:
    /// `com.foo.bar`, `com.foo.bar.plist`, `com.foo.bar.ShipIt` — but not
    /// `com.foo.barista`.
    Prefix,
    /// The token appears anywhere in the name at separator boundaries. Used for
    /// group containers, whose names are `<TEAMID>.<bundle id>`.
    Contains,
}

/// One place to look for leftovers: a directory, the names to look for in it,
/// and how to compare them.
#[derive(Debug, Clone)]
pub struct LeftoverRule {
    /// Human-readable category shown in the UI.
    pub kind: String,
    /// Absolute directory to list.
    pub dir: PathBuf,
    /// Names to match entries against (bundle id and/or display name).
    pub tokens: Vec<String>,
    pub mode: Match,
    /// Whether entries found here are root-owned and need an admin prompt to
    /// remove — true for everything under `/Library`.
    pub requires_admin: bool,
}

/// Whether `name` matches `token` under `mode`. Case-insensitive, because the
/// default macOS filesystem is.
///
/// A match must end on a separator so that one app's identifier does not sweep
/// up another's: `com.anthropic.claude` must not match
/// `com.anthropic.claudefordesktop`.
fn name_matches(name: &str, token: &str, mode: Match) -> bool {
    let name = name.to_lowercase();
    let token = token.to_lowercase();
    if token.len() < 3 {
        return false;
    }
    // Only `.`, `_` and `~` count as boundaries. `-` is deliberately excluded: it
    // is ordinary inside an unrelated name (`claude-cli-nodejs` is not Claude.app),
    // while the suffixes that do belong to an app are dot-joined
    // (`<id>.ShipIt`) or underscore-joined (`Foo_2026-07-17.ips`). `~` is the
    // separator iCloud uses in `Mobile Documents` container names
    // (`<TEAMID>~com~acme~foo`), so an identifier with its dots rewritten to `~`
    // matches there under [`Match::Contains`].
    let is_boundary = |c: char| matches!(c, '.' | '_' | '~');
    let mut from = 0;
    while let Some(rel) = name[from..].find(&token) {
        let start = from + rel;
        let end = start + token.len();
        let before_ok = start == 0
            || (mode == Match::Contains && name[..start].chars().next_back().is_some_and(is_boundary));
        let after_ok = end == name.len() || name[end..].starts_with(is_boundary);
        if before_ok && after_ok {
            return true;
        }
        if mode == Match::Prefix {
            return false;
        }
        from = start + 1;
    }
    false
}

/// The directories searched for an app's leftovers, rooted at `home`.
///
/// Pure: it touches no filesystem. The `/Library` (root domain) rules are only
/// emitted when this build can escalate to root — see
/// [`Capability::AdminEscalation`](crate::capability::Capability::AdminEscalation).
/// Listing a leftover the build could never remove would only produce an
/// uninstall that silently leaves things behind.
pub fn leftover_rules(app_name: &str, bundle_id: Option<&str>, home: &Path) -> Vec<LeftoverRule> {
    leftover_rules_for(&[app_name.to_string()], bundle_id, home)
}

/// [`leftover_rules`] given every name an app answers to.
///
/// An app's support files are not always named after its `.app` filename: a
/// bundle whose file is `Claude Code URL Handler.app` may write its caches under
/// `Claude` (its `CFBundleName`) or its executable name. Matching on the file
/// stem alone is what makes a scan come back nearly empty for such apps, so the
/// caller passes the display name *and* the bundle's `CFBundleName` /
/// `CFBundleExecutable` (see [`identity_names`]). All are treated as equivalent
/// name tokens; the separator-boundary rule in [`name_matches`] keeps them from
/// sweeping up an unrelated app that merely shares a prefix.
pub fn leftover_rules_for(
    names: &[String],
    bundle_id: Option<&str>,
    home: &Path,
) -> Vec<LeftoverRule> {
    let mut rules = user_leftover_rules(names, bundle_id, home);
    if crate::capability::has(crate::capability::Capability::AdminEscalation) {
        rules.extend(system_leftover_rules(names, bundle_id));
    }
    rules
}

/// The identity tokens for an app: its display name plus, when the bundle's
/// `Info.plist` is readable, its `CFBundleName` and `CFBundleExecutable`.
/// Deduplicated case-insensitively; empty and sub-3-character tokens are dropped
/// because [`name_matches`] would ignore them anyway.
pub fn identity_names(app: &AppInfo) -> Vec<String> {
    let mut names = vec![app.name.clone()];
    let info = app.path.join("Contents/Info.plist");
    if let Ok(value) = plist::Value::from_file(info) {
        if let Some(dict) = value.as_dictionary() {
            for key in ["CFBundleName", "CFBundleExecutable"] {
                if let Some(s) = dict.get(key).and_then(|v| v.as_string()) {
                    names.push(s.to_string());
                }
            }
        }
    }
    names.retain(|n| n.chars().count() >= 3);
    // Case-insensitive dedup, preserving first-seen order.
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.to_lowercase()));
    names
}

/// The `~/Library` half of [`leftover_rules_for`]. Always available.
fn user_leftover_rules(names: &[String], bundle_id: Option<&str>, home: &Path) -> Vec<LeftoverRule> {
    let lib = home.join("Library");
    let id = bundle_id.map(str::to_string);

    let both: Vec<String> = id.iter().cloned().chain(names.iter().cloned()).collect();
    let id_only: Vec<String> = id.iter().cloned().collect();
    let name_only: Vec<String> = names.to_vec();
    // iCloud names its `Mobile Documents` containers `<TEAMID>~com~acme~foo`:
    // the bundle identifier with every dot rewritten to a tilde.
    let id_tilde: Vec<String> = id.iter().map(|s| s.replace('.', "~")).collect();

    let mut rules = Vec::new();
    let mut rule = |kind: &str, sub: &str, tokens: &[String], mode: Match| {
        if !tokens.is_empty() {
            rules.push(LeftoverRule {
                kind: kind.to_string(),
                dir: lib.join(sub),
                tokens: tokens.to_vec(),
                mode,
                requires_admin: false,
            });
        }
    };

    // Identifier-keyed locations (the most reliable).
    rule("Preferences", "Preferences", &id_only, Match::Prefix);
    rule("Preferences", "Preferences/ByHost", &id_only, Match::Prefix);
    rule("Containers", "Containers", &id_only, Match::Prefix);
    rule("Group Containers", "Group Containers", &id_only, Match::Contains);
    rule("Saved State", "Saved Application State", &id_only, Match::Prefix);
    rule("HTTP Storage", "HTTPStorages", &id_only, Match::Prefix);
    rule("WebKit", "WebKit", &id_only, Match::Prefix);
    rule("Cookies", "Cookies", &id_only, Match::Prefix);
    rule("App Scripts", "Application Scripts", &id_only, Match::Prefix);
    rule("Launch Agent", "LaunchAgents", &id_only, Match::Prefix);
    rule("iCloud Documents", "Mobile Documents", &id_tilde, Match::Contains);

    // Locations an app may key by either its identifier or its display name.
    rule("Application Support", "Application Support", &both, Match::Prefix);
    rule("Caches", "Caches", &both, Match::Prefix);
    rule("Logs", "Logs", &both, Match::Prefix);
    rule("Autosave", "Autosave Information", &both, Match::Prefix);

    // Name-keyed only: crash reporting writes `<AppName>_<date>.ips`.
    rule("Crash Logs", "Logs/DiagnosticReports", &name_only, Match::Prefix);
    rule(
        "Crash Logs",
        "Application Support/CrashReporter",
        &name_only,
        Match::Prefix,
    );

    // Bundles the app may have installed into the user's Library.
    for sub in [
        "Internet Plug-Ins",
        "PreferencePanes",
        "QuickLook",
        "Screen Savers",
        "Services",
        "Widgets",
        "Audio/Plug-Ins/HAL",
        "Automator",
        "Spelling",
    ] {
        rule("Plug-ins", sub, &both, Match::Prefix);
    }

    rules
}

/// The root-domain directories an app's leftovers may sit in, as
/// `(category, absolute directory, identifier-keyed only)`.
///
/// The third field says whether the display name may be used as a match token
/// too. It is `false` for `LaunchDaemons`, `Preferences` and friends, where a
/// name like "Mail" would be far too broad to match on.
///
/// This table is the single source of truth for two things that must agree: the
/// rules used to *find* root-domain leftovers, and
/// [`is_system_leftover_path`], which decides what the app layer may hand to an
/// escalated removal.
const SYSTEM_LEFTOVER_DIRS: &[(&str, &str, bool)] = &[
    ("Application Support", "/Library/Application Support", false),
    ("Caches", "/Library/Caches", false),
    ("Logs", "/Library/Logs", false),
    // Crash reports for pkg-installed apps and daemons land in the root domain,
    // named `<AppName>_<date>.ips` / `.plist` just like the per-user ones. Both
    // are name-keyed for the same reason the `~/Library` crash rules are: a
    // crash file never carries the bundle identifier.
    ("Crash Logs", "/Library/Logs/DiagnosticReports", false),
    ("Crash Logs", "/Library/Application Support/CrashReporter", false),
    ("Preferences", "/Library/Preferences", true),
    // Background jobs and the helpers they run: the part of an app that keeps
    // working after the bundle is gone, which is why it has to be listed.
    ("Launch Agent", "/Library/LaunchAgents", true),
    ("Launch Daemon", "/Library/LaunchDaemons", true),
    ("Privileged Helper", "/Library/PrivilegedHelperTools", true),
    ("Plug-ins", "/Library/Internet Plug-Ins", false),
    ("Plug-ins", "/Library/PreferencePanes", false),
    ("Plug-ins", "/Library/QuickLook", false),
    ("Plug-ins", "/Library/Screen Savers", false),
    ("Plug-ins", "/Library/Services", false),
    ("Plug-ins", "/Library/Widgets", false),
    ("Plug-ins", "/Library/Audio/Plug-Ins/HAL", false),
    ("Plug-ins", "/Library/Automator", false),
    ("Plug-ins", "/Library/Extensions", false),
    // Installer receipts: a pkg leaves `<id>.bom` and `<id>.plist` here. They
    // are root-owned and identifier-named, so only an exact identifier match
    // nominates them. Removing them is what stops a reinstall from thinking the
    // package is still present.
    ("Installer Receipt", "/private/var/db/receipts", true),
];

/// The `/Library` (root domain) half of [`leftover_rules`]: the files an
/// installer package or a privileged helper wrote outside the user's home.
///
/// Every rule here is marked `requires_admin`, and every directory is a
/// container whose *entries* are named after the app — never the container
/// itself, so a rule can only ever nominate `<known dir>/<app-named entry>`.
fn system_leftover_rules(names: &[String], bundle_id: Option<&str>) -> Vec<LeftoverRule> {
    let id: Vec<String> = bundle_id.map(str::to_string).into_iter().collect();
    let both: Vec<String> = id.iter().cloned().chain(names.iter().cloned()).collect();

    SYSTEM_LEFTOVER_DIRS
        .iter()
        .map(|(kind, dir, id_only)| LeftoverRule {
            kind: (*kind).to_string(),
            dir: PathBuf::from(dir),
            tokens: if *id_only { id.clone() } else { both.clone() },
            mode: Match::Prefix,
            requires_admin: true,
        })
        .filter(|r| !r.tokens.is_empty())
        .collect()
}

/// Whether `path` is something an escalated (root) removal may touch: an entry
/// *directly inside* one of the [`SYSTEM_LEFTOVER_DIRS`].
///
/// The app layer re-checks every path with this before escalating, because the
/// paths arrive back from the frontend and are therefore untrusted. It is what
/// stops an escalated removal from ever naming `/Library`, one of the container
/// directories itself, or anything outside the table.
pub fn is_system_leftover_path(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    // A trailing component is required: `/Library/Caches` has `/Library` as its
    // parent, and must not qualify.
    if path.file_name().is_none() {
        return false;
    }
    SYSTEM_LEFTOVER_DIRS
        .iter()
        .any(|(_, dir, _)| parent == Path::new(dir))
}

/// Find the leftovers for `app` that actually exist on disk, with sizes.
pub fn find_leftovers(app: &AppInfo, home: &Path) -> Vec<Leftover> {
    let names = identity_names(app);
    let rules = leftover_rules_for(&names, app.bundle_id.as_deref(), home);
    let mut seen = std::collections::HashSet::new();
    let mut leftovers = Vec::new();

    for rule in rules {
        // A missing directory is the common case (most apps install into a
        // handful of these), so an unreadable one is not worth reporting.
        let Ok(entries) = std::fs::read_dir(&rule.dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !rule
                .tokens
                .iter()
                .any(|t| name_matches(name, t, rule.mode))
            {
                continue;
            }
            let path = entry.path();
            if !seen.insert(path.clone()) {
                continue;
            }
            let mut warnings = Vec::new();
            let size = if path.is_dir() {
                scanner::dir_size(&path, &mut warnings)
            } else {
                path.metadata().map(|m| m.len()).unwrap_or(0)
            };
            leftovers.push(Leftover {
                kind: rule.kind.clone(),
                path,
                size_bytes: size,
                requires_admin: rule.requires_admin,
            });
        }
    }
    leftovers
}

/// Build the full uninstall plan for `app`.
///
/// The bundle is sized here rather than trusted from `app`, so a plan is exact
/// even when it is built from a listing whose sizes have not landed yet.
pub fn plan_uninstall(mut app: AppInfo, home: &Path) -> UninstallPlan {
    let mut warnings = Vec::new();
    let app_bytes = scanner::dir_size(&app.path, &mut warnings);
    app.size_bytes = Some(app_bytes);
    let leftovers = find_leftovers(&app, home);
    let total_bytes = app_bytes + leftovers.iter().map(|l| l.size_bytes).sum::<u64>();
    UninstallPlan {
        app,
        leftovers,
        total_bytes,
    }
}

/// Whether `path` is an `.app` bundle located directly inside an "Applications"
/// directory. Used by the app layer to gate what uninstall may remove.
pub fn is_app_bundle(path: &Path) -> bool {
    path.extension().map(|e| e == "app").unwrap_or(false)
        && path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n == "Applications")
            .unwrap_or(false)
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
    fn rules_cover_id_keyed_and_name_keyed_locations() {
        let home = PathBuf::from("/Users/me");
        let rules = leftover_rules("Foo", Some("com.acme.foo"), &home);
        let dirs: Vec<String> = rules.iter().map(|r| r.dir.display().to_string()).collect();

        assert!(dirs.iter().any(|d| d.ends_with("Library/Preferences")));
        assert!(dirs.iter().any(|d| d.ends_with("Library/Containers")));
        assert!(dirs.iter().any(|d| d.ends_with("Logs/DiagnosticReports")));

        let support = rules
            .iter()
            .find(|r| r.dir.ends_with("Application Support"))
            .unwrap();
        assert_eq!(support.tokens, vec!["com.acme.foo", "Foo"]);
    }

    #[test]
    fn rules_without_bundle_id_drop_the_id_keyed_directories() {
        let home = PathBuf::from("/Users/me");
        let rules = leftover_rules("Foo", None, &home);
        assert!(rules.iter().all(|r| !r.dir.ends_with("Preferences")));
        let logs = rules.iter().find(|r| r.dir.ends_with("Logs")).unwrap();
        assert_eq!(logs.tokens, vec!["Foo"]);
    }

    #[cfg(feature = "direct")]
    #[test]
    fn system_rules_are_admin_marked_and_never_nominate_library_itself() {
        let rules = leftover_rules("Foo", Some("com.acme.foo"), Path::new("/Users/me"));
        let system: Vec<&LeftoverRule> = rules
            .iter()
            .filter(|r| r.dir.starts_with("/Library"))
            .collect();

        assert!(!system.is_empty());
        assert!(system.iter().all(|r| r.requires_admin));
        // Every scanned directory is a child of /Library, so the entries a rule
        // can nominate are always /Library/<dir>/<entry> — two levels down.
        assert!(system
            .iter()
            .all(|r| r.dir.components().count() > 2 && r.dir != Path::new("/Library")));
        assert!(system.iter().any(|r| r.dir.ends_with("LaunchDaemons")));
        assert!(system.iter().any(|r| r.dir.ends_with("PrivilegedHelperTools")));
        // The root-domain LaunchDaemons/Preferences are identifier-keyed only:
        // a display name like "Mail" is far too broad to match there.
        let daemons = system
            .iter()
            .find(|r| r.dir.ends_with("LaunchDaemons"))
            .unwrap();
        assert_eq!(daemons.tokens, vec!["com.acme.foo"]);
    }

    #[test]
    fn only_entries_inside_a_known_root_domain_directory_may_be_escalated() {
        assert!(is_system_leftover_path(Path::new(
            "/Library/LaunchDaemons/com.acme.foo.plist"
        )));
        assert!(is_system_leftover_path(Path::new(
            "/Library/Application Support/Foo"
        )));
        // Root-domain crash reports, one level below the plain Logs directory.
        assert!(is_system_leftover_path(Path::new(
            "/Library/Logs/DiagnosticReports/Foo_2026-07-17.ips"
        )));
        assert!(is_system_leftover_path(Path::new(
            "/Library/Application Support/CrashReporter/Foo_2026-07-17.plist"
        )));
        // Installer receipts live outside /Library, under /private/var/db.
        assert!(is_system_leftover_path(Path::new(
            "/private/var/db/receipts/com.acme.foo.bom"
        )));
        assert!(!is_system_leftover_path(Path::new(
            "/private/var/db/receipts"
        )));
        // The container directories themselves, and anything above them.
        assert!(!is_system_leftover_path(Path::new("/Library")));
        assert!(!is_system_leftover_path(Path::new("/Library/Caches")));
        assert!(!is_system_leftover_path(Path::new("/")));
        // Nested paths: only the top-level entry is nominated, and the whole
        // subtree goes with it.
        assert!(!is_system_leftover_path(Path::new(
            "/Library/Caches/Foo/inner"
        )));
        // Directories that are not in the table at all.
        assert!(!is_system_leftover_path(Path::new("/Library/Keychains/x")));
        assert!(!is_system_leftover_path(Path::new("/System/Library/Caches/x")));
        assert!(!is_system_leftover_path(Path::new("/Users/me/Library/Caches/x")));
    }

    #[cfg(feature = "mas")]
    #[test]
    fn the_sandboxed_build_stays_inside_the_home_directory() {
        let rules = leftover_rules("Foo", Some("com.acme.foo"), Path::new("/Users/me"));
        assert!(rules.iter().all(|r| r.dir.starts_with("/Users/me")));
        assert!(rules.iter().all(|r| !r.requires_admin));
    }

    #[test]
    fn matching_stops_at_a_separator_boundary() {
        // The suffixed siblings an exact-path probe would miss.
        assert!(name_matches(
            "com.acme.foo.ShipIt",
            "com.acme.foo",
            Match::Prefix
        ));
        assert!(name_matches(
            "com.acme.foo.plist",
            "com.acme.foo",
            Match::Prefix
        ));
        assert!(name_matches("Foo_2026-07-17.ips", "Foo", Match::Prefix));
        assert!(name_matches("COM.ACME.FOO", "com.acme.foo", Match::Prefix));
        // A different app that merely shares a prefix stays untouched.
        assert!(!name_matches(
            "com.acme.foobar",
            "com.acme.foo",
            Match::Prefix
        ));
        assert!(!name_matches("Foobar", "Foo", Match::Prefix));
        // Group containers are team-id prefixed, so the token sits inside.
        assert!(name_matches(
            "AB12CD34.com.acme.foo",
            "com.acme.foo",
            Match::Contains
        ));
        assert!(!name_matches(
            "AB12CD34.com.acme.foobar",
            "com.acme.foo",
            Match::Contains
        ));
        // iCloud `Mobile Documents` containers: team id, then the identifier with
        // its dots rewritten to tildes. The `~` counts as a boundary.
        assert!(name_matches(
            "AB12CD34~com~acme~foo",
            "com~acme~foo",
            Match::Contains
        ));
        assert!(!name_matches(
            "AB12CD34~com~acme~foobar",
            "com~acme~foo",
            Match::Contains
        ));
    }

    #[test]
    fn identity_names_reads_bundle_and_executable_names() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Claude Code URL Handler.app");
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Claude</string>
  <key>CFBundleExecutable</key><string>claude-handler</string>
</dict></plist>"#;
        write_file(&app.join("Contents/Info.plist"), 0);
        fs::write(app.join("Contents/Info.plist"), plist).unwrap();

        let info = AppInfo {
            name: "Claude Code URL Handler".into(),
            path: app,
            bundle_id: Some("com.anthropic.claude-code-url-handler".into()),
            size_bytes: None,
        };
        let names = identity_names(&info);
        // The file stem plus the two Info.plist names, none dropped or duplicated.
        assert_eq!(
            names,
            vec!["Claude Code URL Handler", "Claude", "claude-handler"]
        );
    }

    #[test]
    fn find_leftovers_matches_bundle_name_and_icloud_container() {
        let home = tempfile::tempdir().unwrap();
        let lib = home.path().join("Library");
        // Support directory named after CFBundleName ("Claude"), not the .app file.
        write_file(&lib.join("Application Support/Claude/state.json"), 30);
        // iCloud container: <TEAMID>~<id with tildes>.
        write_file(
            &lib.join("Mobile Documents/AB12CD34~com~acme~handler/doc.txt"),
            20,
        );
        // A different app that merely shares the vendor prefix stays untouched.
        write_file(&lib.join("Application Support/Claudette/x.bin"), 999);

        let bundle = home.path().join("Applications/Claude Code URL Handler.app");
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleName</key><string>Claude</string></dict></plist>"#;
        write_file(&bundle.join("Contents/Info.plist"), 0);
        fs::write(bundle.join("Contents/Info.plist"), plist).unwrap();

        let app = AppInfo {
            name: "Claude Code URL Handler".into(),
            path: bundle,
            bundle_id: Some("com.acme.handler".into()),
            size_bytes: None,
        };
        let found = find_leftovers(&app, home.path());
        let paths: Vec<String> = found.iter().map(|l| l.path.display().to_string()).collect();

        assert!(
            paths.iter().any(|p| p.ends_with("Application Support/Claude")),
            "should match CFBundleName-keyed support dir: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("Mobile Documents/AB12CD34~com~acme~handler")),
            "should match iCloud container: {paths:?}"
        );
        assert!(paths.iter().all(|p| !p.contains("Claudette")), "{paths:?}");
    }

    #[test]
    fn find_leftovers_catches_suffixed_siblings_and_crash_logs() {
        let home = tempfile::tempdir().unwrap();
        let lib = home.path().join("Library");
        write_file(&lib.join("Preferences/com.acme.foo.plist"), 10);
        write_file(&lib.join("Preferences/ByHost/com.acme.foo.ABC.plist"), 10);
        write_file(&lib.join("Caches/com.acme.foo.ShipIt/update.bin"), 10);
        write_file(&lib.join("Group Containers/AB12.com.acme.foo/db"), 10);
        write_file(&lib.join("Logs/DiagnosticReports/Foo_2026-07-17.ips"), 10);
        // Belongs to a different app that shares a prefix.
        write_file(&lib.join("Caches/com.acme.foobar/blob.bin"), 999);

        let app = AppInfo {
            name: "Foo".into(),
            path: PathBuf::from("/Applications/Foo.app"),
            bundle_id: Some("com.acme.foo".into()),
            size_bytes: None,
        };
        let found = find_leftovers(&app, home.path());
        let paths: Vec<String> = found.iter().map(|l| l.path.display().to_string()).collect();

        assert_eq!(found.len(), 5, "found: {paths:?}");
        assert!(paths.iter().all(|p| !p.contains("foobar")));
        assert_eq!(found.iter().map(|l| l.size_bytes).sum::<u64>(), 50);
    }

    #[test]
    fn find_leftovers_keeps_only_existing_and_sizes_them() {
        let home = tempfile::tempdir().unwrap();
        let lib = home.path().join("Library");
        // Two real leftovers, plus candidates that don't exist.
        write_file(&lib.join("Preferences/com.acme.foo.plist"), 40);
        write_file(&lib.join("Caches/com.acme.foo/blob.bin"), 200);

        let app = AppInfo {
            name: "Foo".into(),
            path: PathBuf::from("/Applications/Foo.app"),
            bundle_id: Some("com.acme.foo".into()),
            size_bytes: Some(1000),
        };
        let found = find_leftovers(&app, home.path());
        let total: u64 = found.iter().map(|l| l.size_bytes).sum();
        assert_eq!(found.len(), 2);
        assert_eq!(total, 240);
    }

    #[test]
    fn plan_sizes_the_bundle_itself_and_totals_it_with_leftovers() {
        let home = tempfile::tempdir().unwrap();
        write_file(&home.path().join("Library/Logs/Foo/run.log"), 60);
        let bundle = home.path().join("Applications/Foo.app");
        write_file(&bundle.join("Contents/MacOS/Foo"), 500);

        // An unsized listing entry, as `list_apps` produces.
        let app = AppInfo {
            name: "Foo".into(),
            path: bundle,
            bundle_id: None,
            size_bytes: None,
        };
        let plan = plan_uninstall(app, home.path());
        assert_eq!(plan.app.size_bytes, Some(500));
        assert_eq!(plan.total_bytes, 560);
    }

    #[test]
    fn app_sizes_measures_each_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join("Foo.app");
        let bar = dir.path().join("Bar.app");
        write_file(&foo.join("Contents/MacOS/Foo"), 300);
        write_file(&bar.join("Contents/MacOS/Bar"), 700);

        let sizes = app_sizes(&[foo.clone(), bar.clone()]);
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes.iter().find(|(p, _)| *p == foo).unwrap().1, 300);
        assert_eq!(sizes.iter().find(|(p, _)| *p == bar).unwrap().1, 700);
    }

    #[test]
    fn list_apps_reads_bundle_id_from_info_plist() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Foo.app");
        write_file(&app.join("Contents/MacOS/Foo"), 300);
        // Minimal XML Info.plist with a bundle identifier.
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>com.acme.foo</string>
</dict>
</plist>"#;
        write_file(&app.join("Contents/Info.plist"), 0);
        fs::write(app.join("Contents/Info.plist"), plist).unwrap();

        let apps = list_apps(&[dir.path().to_path_buf()]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Foo");
        assert_eq!(apps[0].bundle_id.as_deref(), Some("com.acme.foo"));
        // Listing is deliberately unsized; `app_sizes` fills this in.
        assert_eq!(apps[0].size_bytes, None);
    }

    #[test]
    fn is_app_bundle_requires_applications_parent() {
        assert!(is_app_bundle(Path::new("/Applications/Foo.app")));
        assert!(is_app_bundle(Path::new("/Users/me/Applications/Foo.app")));
        assert!(!is_app_bundle(Path::new("/Users/me/Downloads/Foo.app")));
        assert!(!is_app_bundle(Path::new("/Applications/Foo")));
    }
}
