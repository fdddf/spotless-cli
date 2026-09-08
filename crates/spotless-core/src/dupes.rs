//! Duplicate file finder.
//!
//! Finds groups of byte-identical files under a root. The search is staged to
//! stay cheap:
//! 1. Bucket files by size — only same-size files can be duplicates.
//! 2. Within each size bucket, hash file contents and group by hash.
//! 3. Confirm each hash group with an exact byte comparison, so a hash
//!    collision can never produce a false "duplicate".
//!
//! Reporting only — this module never mutates the filesystem. The app presents
//! groups and lets the user keep one file per group and Trash the rest.

use std::collections::HashMap;
use std::hash::Hasher;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// A file that participates in a duplicate group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupeFile {
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// A set of byte-identical files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupeGroup {
    /// The shared size of every file in the group.
    pub size_bytes: u64,
    pub files: Vec<DupeFile>,
    /// Bytes reclaimable if all but one copy are removed.
    pub reclaimable_bytes: u64,
}

/// Options bounding the duplicate search.
#[derive(Debug, Clone, Copy)]
pub struct DupeOptions {
    /// Ignore files smaller than this (tiny files aren't worth de-duping).
    pub min_bytes: u64,
}

impl Default for DupeOptions {
    fn default() -> Self {
        // 1 MB floor by default.
        Self {
            min_bytes: 1024 * 1024,
        }
    }
}

/// Hash a file's full contents with a fast non-cryptographic hasher. Used only
/// to cluster candidates; exact equality is confirmed separately by
/// [`files_equal`].
fn hash_file(path: &Path) -> Option<u64> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Some(hasher.finish())
}

/// Byte-for-byte comparison of two files (already known to share a size).
fn files_equal(a: &Path, b: &Path) -> bool {
    let (mut fa, mut fb) = match (std::fs::File::open(a), std::fs::File::open(b)) {
        (Ok(fa), Ok(fb)) => (fa, fb),
        _ => return false,
    };
    let mut ba = [0u8; 64 * 1024];
    let mut bb = [0u8; 64 * 1024];
    loop {
        let na = match fa.read(&mut ba) {
            Ok(n) => n,
            Err(_) => return false,
        };
        let nb = match fb.read(&mut bb) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if na != nb {
            return false;
        }
        if na == 0 {
            return true;
        }
        if ba[..na] != bb[..nb] {
            return false;
        }
    }
}

/// Partition a set of same-size files into groups of byte-identical files.
fn confirm_groups(candidates: Vec<DupeFile>) -> Vec<Vec<DupeFile>> {
    let mut groups: Vec<Vec<DupeFile>> = Vec::new();
    'outer: for file in candidates {
        for group in groups.iter_mut() {
            if files_equal(&group[0].path, &file.path) {
                group.push(file);
                continue 'outer;
            }
        }
        groups.push(vec![file]);
    }
    groups.into_iter().filter(|g| g.len() > 1).collect()
}

/// Find duplicate file groups under `root`, largest reclaimable first.
pub fn find_duplicates(root: &Path, opts: DupeOptions) -> Vec<DupeGroup> {
    // 1. Bucket regular files by size.
    //
    // Staying on one volume matters more here than anywhere else: reached from
    // `/`, every file on the data volume exists under both `/Users/…` and
    // `/System/Volumes/Data/Users/…`, and a duplicate finder that crossed that
    // boundary would offer to delete files as copies of themselves.
    let mounts = crate::mounts::Mounts::read();
    let mut by_size: HashMap<u64, Vec<DupeFile>> = HashMap::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !mounts.is_mount_point(e.path()))
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let size = match entry.metadata() {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if size < opts.min_bytes {
            continue;
        }
        by_size.entry(size).or_default().push(DupeFile {
            path: entry.path().to_path_buf(),
            size_bytes: size,
        });
    }

    // 2 + 3. Within each multi-file size bucket, cluster by hash then confirm.
    let mut result = Vec::new();
    for (size, files) in by_size {
        if files.len() < 2 {
            continue;
        }
        let mut by_hash: HashMap<u64, Vec<DupeFile>> = HashMap::new();
        for f in files {
            if let Some(h) = hash_file(&f.path) {
                by_hash.entry(h).or_default().push(f);
            }
        }
        for (_h, candidates) in by_hash {
            if candidates.len() < 2 {
                continue;
            }
            for mut group in confirm_groups(candidates) {
                group.sort_by(|a, b| a.path.cmp(&b.path));
                let reclaimable = size * (group.len() as u64 - 1);
                result.push(DupeGroup {
                    size_bytes: size,
                    files: group,
                    reclaimable_bytes: reclaimable,
                });
            }
        }
    }

    result.sort_by_key(|a| std::cmp::Reverse(a.reclaimable_bytes));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn groups_identical_files() {
        let dir = tempfile::tempdir().unwrap();
        let content = vec![b'a'; 2048];
        write(&dir.path().join("a.bin"), &content);
        write(&dir.path().join("sub/b.bin"), &content);
        write(&dir.path().join("c.bin"), &content);
        // A unique file of the same size but different content.
        let mut other = vec![b'a'; 2048];
        other[0] = b'z';
        write(&dir.path().join("unique.bin"), &other);

        let opts = DupeOptions { min_bytes: 1 };
        let groups = find_duplicates(dir.path(), opts);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 3);
        assert_eq!(groups[0].size_bytes, 2048);
        // Two copies are redundant.
        assert_eq!(groups[0].reclaimable_bytes, 4096);
    }

    #[test]
    fn same_size_different_content_is_not_a_group() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.bin"), &[1u8; 100]);
        write(&dir.path().join("b.bin"), &[2u8; 100]);

        let opts = DupeOptions { min_bytes: 1 };
        assert!(find_duplicates(dir.path(), opts).is_empty());
    }

    #[test]
    fn respects_min_size() {
        let dir = tempfile::tempdir().unwrap();
        let content = vec![b'x'; 500];
        write(&dir.path().join("a.bin"), &content);
        write(&dir.path().join("b.bin"), &content);

        // 500-byte files are below the 1 KB floor.
        let groups = find_duplicates(dir.path(), DupeOptions { min_bytes: 1024 });
        assert!(groups.is_empty());
    }

    #[test]
    fn files_equal_detects_difference() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let c = dir.path().join("c");
        write(&a, b"hello world");
        write(&b, b"hello world");
        write(&c, b"hello worlZ");
        assert!(files_equal(&a, &b));
        assert!(!files_equal(&a, &c));
    }
}
