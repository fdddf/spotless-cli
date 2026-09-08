//! Disk-usage tree building for the visualizer.
//!
//! One walk produces two things. The [`UsageSnapshot`] is the *whole* tree,
//! every entry, retained in memory; the [`UsageNode`] handed to the UI is a
//! view onto it, expanded only to `max_depth` and keeping the largest
//! `max_children` entries per level with the remainder folded into a single
//! synthetic "Other" node. Deeper subtrees still contribute their full size, so
//! the rings stay proportionally correct without shipping a million nodes over
//! the IPC boundary.
//!
//! The snapshot is why browsing is instant. Building the tree means summing
//! every file under `root`, which on a real home directory is ~1M entries and
//! tens of seconds of pure I/O — so descending into a folder, backing out, and
//! descending again must not re-measure anything. [`UsageSnapshot::view`] cuts
//! a fresh view at any path in the walk without touching the disk.
//!
//! Retaining everything costs memory: an [`Entry`] is 48 bytes plus its name,
//! so a million-entry home directory is on the order of 80 MB. That is the
//! deliberate trade — it buys navigation that never re-walks, and only one
//! snapshot is ever held (each walk replaces the last).
//!
//! The walk itself is parallel (rayon), reports progress as it goes, and can be
//! cancelled between entries.
//!
//! Symlinks are never followed: they are counted as zero-size leaves, matching
//! [`scanner::dir_size`]. Following them would double-count their targets.
//!
//! Neither is any other volume. A walk stays on the filesystem it started on:
//! attached disks, mounted images and network shares appear as zero-byte
//! markers the user can scan separately, and the boot volume's data half is
//! entered only through the firmlinks at `/` rather than a second time through
//! its own mount point. See [`crate::mounts`] for why that boundary cannot be
//! read off `st_dev`.
//!
//! This module never mutates the filesystem.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::mounts::{self, Mounts};

/// A node in the disk-usage tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageNode {
    /// Display name (file/dir name, or "Other" for the folded remainder).
    pub name: String,
    /// Absolute path. Empty for the synthetic "Other" node.
    pub path: PathBuf,
    /// Total size of this subtree in bytes.
    pub size_bytes: u64,
    pub is_dir: bool,
    /// Another volume mounted inside the walk, which was deliberately not
    /// measured: always zero bytes, and an invitation to scan it on its own
    /// rather than something to open or delete.
    pub is_mount: bool,
    /// Child nodes, largest first. Empty for files, folded remainders, and
    /// directories at `max_depth`.
    pub children: Vec<UsageNode>,
}

/// One entry of a retained walk.
///
/// Deliberately smaller than [`UsageNode`]: there is one of these per file on
/// disk, so it carries no absolute path (the path is rebuilt while descending)
/// and boxes its name and children rather than holding growable buffers.
#[derive(Debug)]
struct Entry {
    name: Box<str>,
    size_bytes: u64,
    is_dir: bool,
    /// A separate volume, recorded but not walked.
    is_mount: bool,
    /// Children, largest first. Empty for files and unreadable directories.
    children: Box<[Entry]>,
}

impl Entry {
    fn leaf(name: String, size_bytes: u64, is_dir: bool) -> Self {
        Self {
            name: name.into_boxed_str(),
            size_bytes,
            is_dir,
            is_mount: false,
            children: Box::new([]),
        }
    }

    /// A volume the walk stopped at. Zero bytes because its contents belong to
    /// another disk's total, not this one's.
    fn mount(name: String) -> Self {
        Self {
            is_mount: true,
            // Not `is_dir`: nothing was measured inside it, so offering to
            // descend would only ever show an empty folder.
            ..Self::leaf(name, 0, false)
        }
    }
}

/// A complete walk, kept so the UI can browse it without measuring again.
#[derive(Debug)]
pub struct UsageSnapshot {
    root: PathBuf,
    entry: Entry,
}

impl UsageSnapshot {
    /// The folder this walk covers.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A bounded tree rooted at `path`, or `None` when `path` is outside this
    /// walk. Pure computation over what is already in memory: this is what
    /// makes drilling in and backing out free.
    pub fn view(&self, path: &Path, opts: UsageOptions) -> Option<UsageNode> {
        let entry = self.entry_at(path)?;
        Some(to_node(entry, path, 0, opts))
    }

    /// Size and kind of `path` as the walk recorded them, or `None` when the
    /// walk doesn't cover it.
    ///
    /// This is the membership test behind deleting from the visualizer: a path
    /// the walk never saw is not something the user browsed to and picked.
    ///
    /// A mounted volume counts as never seen. It is in the tree as a marker,
    /// with none of its contents measured, and "delete" on a mount point is
    /// never what someone means.
    pub fn info(&self, path: &Path) -> Option<UsageInfo> {
        let entry = self.entry_at(path).filter(|e| !e.is_mount)?;
        Some(UsageInfo {
            size_bytes: entry.size_bytes,
            is_dir: entry.is_dir,
        })
    }

    /// Forget `path`, as if the walk had never seen it, and return the size
    /// that went with it.
    ///
    /// Called after something is actually deleted: the walk is now wrong about
    /// the disk, and re-walking to correct it would undo the whole point of
    /// keeping it. Dropping the entry and taking its bytes off every folder
    /// above it leaves the same tree the next walk would produce.
    ///
    /// The root itself cannot be forgotten — there would be no snapshot left.
    pub fn remove(&mut self, path: &Path) -> Option<u64> {
        let relative: Vec<&str> = path
            .strip_prefix(&self.root)
            .ok()?
            .components()
            .filter_map(|c| match c {
                Component::Normal(n) => Some(n.to_str()),
                _ => None,
            })
            .collect::<Option<Vec<&str>>>()?;
        if relative.is_empty() {
            return None;
        }
        remove_in(&mut self.entry, &relative)
    }

    /// Resolve a path to its entry by descending name by name. Names within a
    /// directory are unique, so the walk down is unambiguous.
    fn entry_at(&self, path: &Path) -> Option<&Entry> {
        let mut entry = &self.entry;
        for component in path.strip_prefix(&self.root).ok()?.components() {
            let name = match component {
                Component::Normal(n) => n.to_str()?,
                // A relative step is either a no-op or an escape upwards; the
                // latter is not something a UI path should ever contain.
                Component::CurDir => continue,
                _ => return None,
            };
            entry = entry.children.iter().find(|c| &*c.name == name)?;
        }
        Some(entry)
    }
}

/// What the walk recorded about one path.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UsageInfo {
    pub size_bytes: u64,
    pub is_dir: bool,
}

/// Drop the entry `relative` names and take its bytes off every folder above it.
fn remove_in(entry: &mut Entry, relative: &[&str]) -> Option<u64> {
    let (name, rest) = relative.split_first()?;
    let at = entry.children.iter().position(|c| &*c.name == *name)?;

    let freed = if rest.is_empty() {
        // Boxed slices can't shrink in place, so this rebuilds the one
        // directory's children — order is preserved, so it stays largest-first.
        let mut children = std::mem::take(&mut entry.children).into_vec();
        let gone = children.remove(at);
        entry.children = children.into_boxed_slice();
        gone.size_bytes
    } else {
        remove_in(&mut entry.children[at], rest)?
    };

    entry.size_bytes = entry.size_bytes.saturating_sub(freed);
    Some(freed)
}

/// Cut a bounded [`UsageNode`] out of a retained subtree.
fn to_node(entry: &Entry, path: &Path, depth: usize, opts: UsageOptions) -> UsageNode {
    // At the depth limit a directory becomes a sized leaf: the size is already
    // known, only the enumeration stops.
    let mut children = Vec::new();
    if entry.is_dir && depth < opts.max_depth {
        let keep = entry.children.len().min(opts.max_children);
        children = entry.children[..keep]
            .iter()
            .map(|c| to_node(c, &path.join(&*c.name), depth + 1, opts))
            .collect();

        // Volumes are zero-byte by construction, so they sort last and would
        // always disappear into the fold — and a fold of nothing but zeroes is
        // not even emitted. They are lifted out of it so that "there is another
        // disk mounted here" survives into the view.
        let (mounts, folded): (Vec<&Entry>, Vec<&Entry>) =
            entry.children[keep..].iter().partition(|c| c.is_mount);
        children.extend(
            mounts
                .iter()
                .map(|c| to_node(c, &path.join(&*c.name), depth + 1, opts)),
        );

        let folded_size: u64 = folded.iter().map(|c| c.size_bytes).sum();
        if folded_size > 0 {
            children.push(UsageNode {
                name: format!("Other ({} items)", folded.len()),
                path: PathBuf::new(),
                size_bytes: folded_size,
                is_dir: false,
                is_mount: false,
                children: Vec::new(),
            });
        }
    }

    UsageNode {
        name: entry.name.to_string(),
        path: path.to_path_buf(),
        size_bytes: entry.size_bytes,
        is_dir: entry.is_dir,
        is_mount: entry.is_mount,
        children,
    }
}

/// How the tree is bounded.
#[derive(Debug, Clone, Copy)]
pub struct UsageOptions {
    /// Maximum directory depth to expand (root is depth 0).
    pub max_depth: usize,
    /// Maximum real children to keep per level before folding into "Other".
    pub max_children: usize,
}

impl Default for UsageOptions {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_children: 20,
        }
    }
}

/// How far along a running tree build is. Directory count is the only honest
/// progress signal available — the total isn't known until the walk finishes,
/// so this drives a spinner with live counters, not a percentage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UsageProgress {
    /// Directories entered so far.
    pub dirs_scanned: u64,
    /// Bytes summed so far.
    pub bytes_seen: u64,
}

/// What the walk does with a directory that turns out to be a mount point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crossing {
    /// Not a boundary at all: an ordinary folder on this volume.
    Enter,
    /// The boot volume's data half. Entered, because reaching it means the walk
    /// started on the system half and there is content here that no firmlink at
    /// `/` leads to — but everything that *is* firmlinked has been counted
    /// already, so from here down each folder is checked against its mirror.
    EnterDataVolume,
    /// Another volume: recorded as a marker, not measured.
    Skip,
}

/// Shared state for one walk: counters, the volume boundary, and the caller's
/// progress/cancel hooks.
struct Walk<'a> {
    dirs: AtomicU64,
    bytes: AtomicU64,
    /// Where other filesystems begin, read once when the walk starts.
    mounts: Mounts,
    on_progress: &'a (dyn Fn(UsageProgress) + Sync),
    cancel: &'a (dyn Fn() -> bool + Sync),
}

impl Walk<'_> {
    /// Whether descending into `path` would leave the volume being measured.
    fn crossing(&self, path: &Path) -> Crossing {
        if !self.mounts.is_mount_point(path) {
            Crossing::Enter
        } else if path == Path::new(mounts::DATA_VOLUME) {
            Crossing::EnterDataVolume
        } else {
            Crossing::Skip
        }
    }

    /// Count a directory, emitting progress every so often. The callback
    /// crosses an IPC channel, so it is throttled rather than fired per entry.
    fn enter_dir(&self) {
        let n = self.dirs.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(256) {
            (self.on_progress)(UsageProgress {
                dirs_scanned: n,
                bytes_seen: self.bytes.load(Ordering::Relaxed),
            });
        }
    }

    fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        (self.cancel)()
    }

    fn progress(&self) -> UsageProgress {
        UsageProgress {
            dirs_scanned: self.dirs.load(Ordering::Relaxed),
            bytes_seen: self.bytes.load(Ordering::Relaxed),
        }
    }
}

/// Build a usage tree rooted at `root`. Walks the whole subtree; see
/// [`build_usage_snapshot`] for the cancellable, progress-reporting form.
pub fn build_usage_tree(root: &Path, opts: UsageOptions) -> UsageNode {
    build_usage_snapshot(root, &|_| {}, &|| false)
        .expect("a walk that is never cancelled always yields a snapshot")
        .view(root, opts)
        .expect("a snapshot always contains its own root")
}

/// Walk `root`, reporting progress via `on_progress` and stopping early when
/// `cancel` returns true. Returns `None` if the walk was cancelled.
///
/// The result is the whole tree: bound it into something renderable with
/// [`UsageSnapshot::view`], which can be called again for any path inside it
/// without walking anything a second time.
pub fn build_usage_snapshot(
    root: &Path,
    on_progress: &(dyn Fn(UsageProgress) + Sync),
    cancel: &(dyn Fn() -> bool + Sync),
) -> Option<UsageSnapshot> {
    let walk = Walk {
        dirs: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        mounts: Mounts::read(),
        on_progress,
        cancel,
    };
    // The root is whatever the user picked, so follow it even if it's a link,
    // and measure it even though it may itself be a mount point — "scan this
    // volume" is exactly what picking one means. Picking the data volume that
    // way measures all of it, firmlinks included: this walk has no `/` above it
    // to have counted them.
    let entry = build_entry(root, node_name(root), root.is_dir(), &walk, None);
    if walk.cancelled() {
        return None;
    }
    // Final tick, so the UI's last numbers match the finished tree.
    on_progress(walk.progress());
    Some(UsageSnapshot {
        root: root.to_path_buf(),
        entry,
    })
}

fn node_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Name of a `read_dir` entry, without going back to the filesystem.
fn entry_name(entry: &std::fs::DirEntry) -> String {
    entry.file_name().to_string_lossy().into_owned()
}

/// Measure one entry and everything under it, retaining the result.
///
/// The name is passed in because the caller already has it from `read_dir`;
/// deriving it from the path again would mean another allocation per file.
///
/// `mirror` is where this directory also appears under `/`, set while walking
/// inside the data volume and `None` everywhere else; see
/// [`Crossing::EnterDataVolume`].
fn build_entry(
    path: &Path,
    name: String,
    is_dir: bool,
    walk: &Walk,
    mirror: Option<&Path>,
) -> Entry {
    if walk.cancelled() {
        return Entry::leaf(name, 0, is_dir);
    }

    // Files (and anything we can't descend) are leaves.
    if !is_dir {
        let size = path.metadata().map(|m| m.len()).unwrap_or(0);
        walk.add_bytes(size);
        return Entry::leaf(name, size, false);
    }

    walk.enter_dir();

    // Descend one level: build each child in parallel, then order by size.
    // `read_dir` already knows each entry's name and type, so both are passed
    // down rather than re-deriving them per entry.
    let entries: Vec<_> = match std::fs::read_dir(path) {
        Ok(e) => e.filter_map(|x| x.ok()).collect(),
        Err(_) => Vec::new(),
    };
    let mut children: Vec<Entry> = entries
        .par_iter()
        .filter_map(|e| {
            let ft = e.file_type().ok();
            // A symlink is neither descended nor sized: following it would
            // double-count its target.
            if ft.map(|t| t.is_symlink()).unwrap_or(false) {
                return Some(Entry::leaf(entry_name(e), 0, false));
            }
            let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
            let child = e.path();
            if !is_dir {
                return Some(build_entry(&child, entry_name(e), false, walk, None));
            }

            // Inside the data volume, decide whether this folder is one `/`
            // already led to. If it is, it is not listed again — dropped rather
            // than zeroed, because it is not something that lives here, it is
            // the same folder under a second name. If it isn't, its children
            // may still be, so the mirror follows the walk down; and once the
            // mirror stops existing, nothing below can be firmlinked and the
            // checking stops with it.
            let mirror = mirror.map(|m| m.join(entry_name(e)));
            let below = match mirror.as_deref().map(|m| (m, mounts::mirrored(&child, m))) {
                Some((_, mounts::Mirrored::Same)) => return None,
                Some((m, mounts::Mirrored::Different)) => Some(m),
                Some((_, mounts::Mirrored::Absent)) | None => None,
            };

            Some(match walk.crossing(&child) {
                Crossing::Skip => Entry::mount(entry_name(e)),
                // Every folder below here is checked against the same path
                // under `/`, which is where its firmlink would surface.
                Crossing::EnterDataVolume => {
                    build_entry(&child, entry_name(e), true, walk, Some(Path::new("/")))
                }
                Crossing::Enter => build_entry(&child, entry_name(e), true, walk, below),
            })
        })
        .collect();

    // Sorted once, here, so every view cut from the snapshot is largest-first
    // without sorting again.
    children.sort_by_key(|c| std::cmp::Reverse(c.size_bytes));

    Entry {
        name: name.into_boxed_str(),
        size_bytes: children.iter().map(|c| c.size_bytes).sum(),
        is_dir: true,
        is_mount: false,
        children: children.into_boxed_slice(),
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
    fn sizes_roll_up_and_children_sorted_desc() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("big/a.bin"), 500);
        write_file(&dir.path().join("small/b.bin"), 50);

        let tree = build_usage_tree(dir.path(), UsageOptions::default());
        assert_eq!(tree.size_bytes, 550);
        assert!(tree.is_dir);
        assert_eq!(tree.children.len(), 2);
        // Largest first.
        assert_eq!(tree.children[0].name, "big");
        assert_eq!(tree.children[0].size_bytes, 500);
        assert_eq!(tree.children[1].name, "small");
    }

    #[test]
    fn depth_limit_keeps_size_but_not_children() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b/c/deep.bin"), 123);

        let opts = UsageOptions {
            max_depth: 1,
            max_children: 20,
        };
        let tree = build_usage_tree(dir.path(), opts);
        // root (0) -> a (1, at limit): a is a sized leaf, no children expanded.
        assert_eq!(tree.size_bytes, 123);
        let a = &tree.children[0];
        assert_eq!(a.name, "a");
        assert_eq!(a.size_bytes, 123);
        assert!(a.children.is_empty());
    }

    #[test]
    fn excess_children_fold_into_other() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            // Descending sizes so ordering is deterministic.
            write_file(&dir.path().join(format!("f{i}.bin")), 100 - i * 10);
        }

        let opts = UsageOptions {
            max_depth: 2,
            max_children: 3,
        };
        let tree = build_usage_tree(dir.path(), opts);
        // 3 kept + 1 "Other" fold = 4 nodes.
        assert_eq!(tree.children.len(), 4);
        let other = tree.children.last().unwrap();
        assert!(other.name.starts_with("Other ("));
        // Folded remainder = f3 (70) + f4 (60) = 130.
        assert_eq!(other.size_bytes, 130);
        // Total is unaffected by folding.
        assert_eq!(tree.size_bytes, 100 + 90 + 80 + 70 + 60);
    }

    #[test]
    fn symlinked_dir_is_not_followed_or_counted() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("real/big.bin"), 500);
        // A link back to a sibling: following it would double-count "real".
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();

        let tree = build_usage_tree(dir.path(), UsageOptions::default());
        assert_eq!(tree.size_bytes, 500, "symlink target must not be counted");
        let link = tree.children.iter().find(|c| c.name == "link").unwrap();
        assert_eq!(link.size_bytes, 0);
        assert!(link.children.is_empty());
    }

    #[test]
    fn cancelled_walk_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b.bin"), 100);

        let out = build_usage_snapshot(dir.path(), &|_| {}, &|| true);
        assert!(out.is_none());
    }

    #[test]
    fn snapshot_views_below_the_depth_limit_without_walking_again() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b/c/d/deep.bin"), 700);
        write_file(&dir.path().join("a/b/c/d/small.bin"), 7);

        let snapshot = build_usage_snapshot(dir.path(), &|_| {}, &|| false).unwrap();
        let opts = UsageOptions {
            max_depth: 1,
            max_children: 20,
        };

        // The tree the UI first sees stops at `a`, as the depth limit says.
        let shallow = snapshot.view(dir.path(), opts).unwrap();
        assert!(shallow.children[0].children.is_empty());

        // Drilling to a folder the first view never enumerated is still served
        // from the same walk — including its ordering by size.
        let deep = snapshot.view(&dir.path().join("a/b/c/d"), opts).unwrap();
        assert_eq!(deep.name, "d");
        assert_eq!(deep.size_bytes, 707);
        let names: Vec<&str> = deep.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["deep.bin", "small.bin"]);
        // Views carry absolute paths, rebuilt on the way down.
        assert_eq!(deep.children[0].path, dir.path().join("a/b/c/d/deep.bin"));
    }

    #[test]
    fn snapshot_rejects_paths_outside_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b.bin"), 100);

        let snapshot = build_usage_snapshot(&dir.path().join("a"), &|_| {}, &|| false).unwrap();
        let opts = UsageOptions::default();
        assert!(snapshot.view(dir.path(), opts).is_none(), "above the root");
        assert!(
            snapshot.view(&dir.path().join("a/nope"), opts).is_none(),
            "no such child"
        );
        assert!(snapshot.view(&dir.path().join("a"), opts).is_some());
    }

    #[test]
    fn removing_an_entry_shrinks_every_folder_above_it() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b/big.bin"), 900);
        write_file(&dir.path().join("a/b/keep.bin"), 30);
        write_file(&dir.path().join("c.bin"), 70);

        let mut snapshot = build_usage_snapshot(dir.path(), &|_| {}, &|| false).unwrap();
        assert_eq!(snapshot.remove(&dir.path().join("a/b/big.bin")), Some(900));

        let opts = UsageOptions::default();
        // The bytes come off the root, the folder, and everything between.
        assert_eq!(snapshot.view(dir.path(), opts).unwrap().size_bytes, 100);
        let b = snapshot.view(&dir.path().join("a/b"), opts).unwrap();
        assert_eq!(b.size_bytes, 30);
        let names: Vec<&str> = b.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["keep.bin"], "the removed entry is gone");
        // Siblings elsewhere are untouched.
        assert_eq!(
            snapshot.info(&dir.path().join("c.bin")).unwrap().size_bytes,
            70
        );
    }

    #[test]
    fn removing_a_folder_takes_its_whole_subtree() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b/deep.bin"), 500);
        write_file(&dir.path().join("c.bin"), 60);

        let mut snapshot = build_usage_snapshot(dir.path(), &|_| {}, &|| false).unwrap();
        assert_eq!(snapshot.remove(&dir.path().join("a")), Some(500));
        assert_eq!(
            snapshot.view(dir.path(), UsageOptions::default()).unwrap().size_bytes,
            60
        );
        assert!(snapshot.info(&dir.path().join("a/b/deep.bin")).is_none());
    }

    #[test]
    fn removing_refuses_the_root_and_unknown_paths() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a.bin"), 10);

        let mut snapshot = build_usage_snapshot(dir.path(), &|_| {}, &|| false).unwrap();
        // Forgetting the root would leave no snapshot to browse.
        assert_eq!(snapshot.remove(dir.path()), None);
        assert_eq!(snapshot.remove(&dir.path().join("nope.bin")), None);
        assert_eq!(snapshot.remove(&PathBuf::from("/elsewhere")), None);
        // A refused removal changes nothing.
        assert_eq!(
            snapshot.view(dir.path(), UsageOptions::default()).unwrap().size_bytes,
            10
        );
    }

    /// A walk that measures nothing, for testing the boundary rules alone.
    fn probe_walk() -> Walk<'static> {
        Walk {
            dirs: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            mounts: Mounts::read(),
            on_progress: &|_| {},
            cancel: &|| false,
        }
    }

    #[test]
    fn attached_volumes_are_recorded_but_not_walked() {
        let seen = AtomicU64::new(0);
        // Should the boundary fail to hold, this walks whatever disk is plugged
        // in — minutes of I/O for a failing test — so it is cut off early and
        // being cut off is itself the failure.
        let snapshot = build_usage_snapshot(
            Path::new("/Volumes"),
            &|p| seen.store(p.dirs_scanned, Ordering::Relaxed),
            &|| seen.load(Ordering::Relaxed) > 64,
        )
        .expect("/Volumes must not descend into the disks mounted under it");

        let tree = snapshot
            .view(Path::new("/Volumes"), UsageOptions::default())
            .unwrap();
        let mounts = Mounts::read();
        // Machines with nothing attached have only the boot volume's symlink
        // here, and then this asserts nothing — the walk finishing at all is
        // the interesting half.
        for child in tree.children.iter().filter(|c| mounts.is_mount_point(&c.path)) {
            assert!(child.is_mount, "{} is another volume", child.path.display());
            assert_eq!(child.size_bytes, 0, "its bytes belong to its own total");
            assert!(child.children.is_empty(), "and nothing under it was walked");
        }
    }

    #[test]
    fn the_boot_volume_group_is_entered_and_other_volumes_are_not() {
        let data = Path::new(mounts::DATA_VOLUME);
        if !data.exists() {
            return; // Pre-Catalina layout: no volume group to split.
        }
        let walk = probe_walk();

        // The data half is entered rather than skipped: some of it has no
        // firmlink at `/`, and that part is only reachable here.
        assert_eq!(walk.crossing(data), Crossing::EnterDataVolume);
        // Ordinary directories are not boundaries at all.
        assert_eq!(walk.crossing(Path::new("/System/Volumes")), Crossing::Enter);
        assert_eq!(walk.crossing(Path::new("/Users")), Crossing::Enter);
        // Anything else mounted is another disk's business.
        for other in ["/System/Volumes/Preboot", "/System/Volumes/VM", "/dev"] {
            let other = Path::new(other);
            if other.exists() {
                assert_eq!(walk.crossing(other), Crossing::Skip, "{}", other.display());
            }
        }
    }

    #[test]
    fn nothing_the_data_volume_shares_with_root_is_walked_twice() {
        let data = Path::new(mounts::DATA_VOLUME);
        if !data.exists() {
            return;
        }
        // What the data volume holds that `/` does not already show is a few
        // GB in a few thousand directories: `MobileSoftwareUpdate`, the
        // Spotlight index, the revisions store. Should a firmlink slip through,
        // this is the entire home directory instead — so the walk is cut off an
        // order of magnitude above what it should need, and being cut off is
        // the failure.
        let dirs = AtomicU64::new(0);
        let snapshot = build_usage_snapshot(
            Path::new("/System/Volumes"),
            &|p| dirs.store(p.dirs_scanned, Ordering::Relaxed),
            &|| dirs.load(Ordering::Relaxed) > 100_000,
        )
        .expect("a firmlinked folder was walked a second time inside the data volume");

        /// Assert of every directory that it is not one `/` already leads to.
        fn only_its_own(node: &UsageNode) {
            if let Ok(relative) = node.path.strip_prefix(mounts::DATA_VOLUME) {
                let at_root = Path::new("/").join(relative);
                assert_ne!(
                    mounts::mirrored(&node.path, &at_root),
                    mounts::Mirrored::Same,
                    "{} is {} under a second name",
                    node.path.display(),
                    at_root.display()
                );
            }
            node.children.iter().for_each(only_its_own);
        }

        only_its_own(
            &snapshot
                .view(
                    data,
                    UsageOptions {
                        max_depth: 3,
                        max_children: usize::MAX,
                    },
                )
                .expect("the data volume is inside the walk"),
        );
    }

    #[test]
    fn a_volume_marker_survives_the_fold_and_cannot_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("big.bin"), 100);

        // Hand-built rather than mounted: the fold and the delete guard are
        // decisions about the tree, not about the filesystem.
        let mut snapshot = build_usage_snapshot(dir.path(), &|_| {}, &|| false).unwrap();
        let mut children = std::mem::take(&mut snapshot.entry.children).into_vec();
        children.push(Entry::mount("Backup Disk".into()));
        snapshot.entry.children = children.into_boxed_slice();

        let opts = UsageOptions {
            max_depth: 2,
            // One slot, which the 100-byte file takes: a zero-byte volume can
            // only ever be the entry that gets folded away.
            max_children: 1,
        };
        let tree = snapshot.view(dir.path(), opts).unwrap();
        let volume = tree
            .children
            .iter()
            .find(|c| c.name == "Backup Disk")
            .expect("the volume is listed even though it sorts last");
        assert!(volume.is_mount);
        assert_eq!(tree.size_bytes, 100, "and adds nothing to the total");

        // Trashing works off `info`, so refusing here is what stops a mount
        // point from being handed to the cleaner.
        assert!(snapshot.info(&dir.path().join("Backup Disk")).is_none());
        assert!(snapshot.info(&dir.path().join("big.bin")).is_some());
    }

    #[test]
    fn progress_reports_dirs_and_bytes_on_completion() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b.bin"), 100);
        write_file(&dir.path().join("c.bin"), 23);

        let last = std::sync::Mutex::new(None);
        let snapshot =
            build_usage_snapshot(dir.path(), &|p| *last.lock().unwrap() = Some(p), &|| false)
                .unwrap();

        let tree = snapshot.view(dir.path(), UsageOptions::default()).unwrap();
        assert_eq!(tree.size_bytes, 123);
        // The completion tick always fires, even below the throttle interval.
        let p = last.lock().unwrap().expect("progress must be reported");
        assert_eq!(p.bytes_seen, 123);
        assert!(p.dirs_scanned >= 2, "root and a/ were entered");
    }
}
