//! Loading of the data-driven cleaning ruleset.
//!
//! Rules are TOML documents containing a list of `[[target]]` tables that
//! deserialize into [`ScanTarget`]. Keeping targets as data (rather than code)
//! is a core product principle: the complete set of things Spotless can ever
//! remove is reviewable and versionable.

use crate::capability;
use crate::model::ScanTarget;
use crate::CoreError;

/// A parsed ruleset.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct RuleSet {
    #[serde(default, rename = "target")]
    pub targets: Vec<ScanTarget>,
}

impl RuleSet {
    /// Parse a ruleset from a TOML string.
    ///
    /// Targets declaring a [`Capability`](crate::capability::Capability) this
    /// build lacks are dropped here, so callers never have to think about the
    /// variant — a target that reached them is one they may act on. Validation
    /// runs against the *full* set first, so a duplicate id is still an error
    /// even when one of the pair is about to be filtered out.
    pub fn from_toml(toml_str: &str) -> Result<Self, CoreError> {
        let mut set: RuleSet = toml::from_str(toml_str)?;
        set.validate()?;
        set.targets
            .retain(|t| t.requires.is_none_or(capability::has));
        Ok(set)
    }

    /// Load and merge every `*.toml` ruleset in a directory (sorted by name for
    /// deterministic ordering).
    pub fn from_dir(dir: &std::path::Path) -> Result<Self, CoreError> {
        let mut files: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
            .collect();
        files.sort();

        let mut merged = RuleSet::default();
        for file in files {
            let text = std::fs::read_to_string(&file)?;
            let set = RuleSet::from_toml(&text)?;
            merged.targets.extend(set.targets);
        }
        merged.validate()?;
        Ok(merged)
    }

    /// Ensure target ids are unique and non-empty.
    fn validate(&self) -> Result<(), CoreError> {
        let mut seen = std::collections::HashSet::new();
        for t in &self.targets {
            if t.id.trim().is_empty() {
                return Err(CoreError::Rule("a target has an empty id".into()));
            }
            if !seen.insert(&t.id) {
                return Err(CoreError::Rule(format!("duplicate target id: {}", t.id)));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;
    use crate::model::{Category, SafetyTier, Scope};

    const SAMPLE: &str = r#"
[[target]]
id = "user-caches"
name = "User application caches"
path = "~/Library/Caches"
scope = "contents"
safety = "safe"
category = "user-cache"
requires_app_quit = true
description = "App caches; regenerated on next launch."

[[target]]
id = "xcode-derived-data"
name = "Xcode DerivedData"
path = "~/Library/Developer/Xcode/DerivedData"
category = "developer"
"#;

    #[test]
    fn parses_targets_with_defaults() {
        let set = RuleSet::from_toml(SAMPLE).unwrap();
        assert_eq!(set.targets.len(), 2);

        let caches = &set.targets[0];
        assert_eq!(caches.id, "user-caches");
        assert_eq!(caches.scope, Scope::Contents);
        assert_eq!(caches.safety, SafetyTier::Safe);
        assert_eq!(caches.category, Category::UserCache);
        assert!(caches.requires_app_quit);

        // Second target relies on serde defaults for the omitted fields.
        let xcode = &set.targets[1];
        assert_eq!(xcode.scope, Scope::Contents); // default
        assert_eq!(xcode.safety, SafetyTier::Caution); // default
        assert_eq!(xcode.category, Category::Developer);
        assert!(!xcode.requires_app_quit); // default false
    }

    #[test]
    fn keeps_targets_without_a_capability_requirement() {
        // Neither SAMPLE target declares `requires`, so both survive in either
        // build — the common case, and the one that must not regress.
        assert_eq!(RuleSet::from_toml(SAMPLE).unwrap().targets.len(), 2);
    }

    #[test]
    fn filters_targets_by_capability() {
        let gated = r#"
[[target]]
id = "always"
name = "Always"
path = "~/a"

[[target]]
id = "root-caches"
name = "Root caches"
path = "/Library/Caches"
requires = "system-caches"
"#;
        let set = RuleSet::from_toml(gated).unwrap();
        let ids: Vec<&str> = set.targets.iter().map(|t| t.id.as_str()).collect();

        if capability::has(Capability::SystemCaches) {
            assert_eq!(ids, ["always", "root-caches"]);
        } else {
            assert_eq!(ids, ["always"]);
        }
    }

    #[test]
    fn duplicate_ids_are_rejected_before_filtering() {
        // Filtering must not be able to mask a malformed ruleset: a duplicate
        // id is an authoring error regardless of which build is loading it.
        let dup = r#"
[[target]]
id = "dup"
name = "A"
path = "~/a"
requires = "system-caches"
[[target]]
id = "dup"
name = "B"
path = "~/b"
"#;
        assert!(RuleSet::from_toml(dup).is_err());
    }

    #[test]
    fn rejects_duplicate_ids() {
        let dup = r#"
[[target]]
id = "dup"
name = "A"
path = "~/a"
[[target]]
id = "dup"
name = "B"
path = "~/b"
"#;
        assert!(RuleSet::from_toml(dup).is_err());
    }
}
