//! The mount table, so a walk can tell "another volume" from "another folder".
//!
//! A disk-usage walk that follows every directory it meets does not measure a
//! disk, it measures everything currently reachable from a path: attached
//! external drives, mounted disk images (Xcode's simulator runtimes are one
//! each), network shares, and macOS's own synthetic volumes. Sizing "Macintosh
//! HD" should not include the 900 GB drive someone plugged in.
//!
//! macOS makes the boundary less obvious than a device-id comparison would
//! suggest, in both directions:
//!
//! * The boot disk is a *volume group*: a read-only system volume mounted at
//!   `/` and a writable data volume mounted at [`DATA_VOLUME`]. `/Users`,
//!   `/Applications` and `/private` are **firmlinks** into the data volume —
//!   real directories, not symlinks, that a walk descends without noticing. So
//!   walking `/` reaches the whole data volume twice, once through the
//!   firmlinks and once through the mount, and doubles the machine's used
//!   space. Worse, the two report the *same* `st_dev`, so comparing device ids
//!   parent-to-child does not catch it (and would wrongly cut `/Users` off if
//!   it did).
//! * Every other mount does change `st_dev`, but so would a firmlink if Apple
//!   had not hidden it — which is why this module asks the kernel for the mount
//!   table rather than inferring boundaries from `stat`.
//!
//! The table is read once per walk and consulted by path, so the walk itself
//! costs no extra syscalls.

use std::collections::HashSet;
use std::ffi::CStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Where the boot volume group's writable half is mounted.
///
/// Stable since Catalina (10.15), which is when the read-only system volume and
/// firmlinks arrived. Not being able to find it is not an error: it only means
/// the walk treats it as an ordinary separate volume and skips it.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The filesystems mounted right now, by mount point.
#[derive(Debug, Default, Clone)]
pub struct Mounts {
    points: HashSet<PathBuf>,
}

impl Mounts {
    /// Read the kernel's mount table, once, for a walk to consult by path.
    ///
    /// `getfsstat` rather than the friendlier `getmntinfo`: the latter returns
    /// a static buffer it reuses across callers, so two walks starting at the
    /// same moment race each other and one of them sees an empty table. This
    /// fills a buffer of our own.
    ///
    /// A table that can't be read is not an error — [`Self::is_mount_point`]
    /// falls back to asking about one path at a time.
    pub fn read() -> Self {
        // SAFETY: a null buffer with zero size asks only for the count.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Self::default();
        }

        // Slack for anything mounted between the two calls; the kernel fills at
        // most what the size says and reports how many it wrote.
        let capacity = count as usize + 8;
        let Ok(size) = (capacity * std::mem::size_of::<libc::statfs>()).try_into() else {
            return Self::default();
        };
        let mut buf: Vec<libc::statfs> = Vec::with_capacity(capacity);
        // SAFETY: `buf` owns room for `capacity` entries and `size` describes
        // exactly that, so the kernel writes within it.
        let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        if n <= 0 {
            return Self::default();
        }
        // SAFETY: `statfs` is plain data with no drop glue, and the kernel
        // initialised the `n` entries now claimed.
        unsafe { buf.set_len((n as usize).min(capacity)) };

        let points = buf.iter().filter_map(mount_point).collect();
        Self { points }
    }

    /// True when `path` is itself where a filesystem is mounted — the point at
    /// which descending leaves the volume the walk started on.
    ///
    /// Firmlinked directories are not mount points, so `/Users` is false here
    /// and is walked as the ordinary folder the user thinks it is.
    pub fn is_mount_point(&self, path: &Path) -> bool {
        if self.points.is_empty() {
            // No table: ask the filesystem about this one path. A syscall per
            // directory is worse than a hash lookup, but it is the difference
            // between the boundary holding and not.
            return mounted_at(path);
        }
        self.points.contains(path)
    }

    /// How many filesystems were found. Zero means the table could not be read.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// Where one filesystem is mounted, as an owned path.
fn mount_point(fs: &libc::statfs) -> Option<PathBuf> {
    // SAFETY: `f_mntonname` is a NUL-terminated path the kernel wrote into the
    // fixed-size field, and it lives as long as the borrow of `fs`.
    let name = unsafe { CStr::from_ptr(fs.f_mntonname.as_ptr()) };
    name.to_str().ok().map(PathBuf::from)
}

/// Whether a filesystem is mounted exactly at `path`, asked of `path` itself.
///
/// The fallback for a missing mount table: `statfs` reports the mount the path
/// belongs to, and a path that *is* a mount point is its own.
fn mounted_at(path: &Path) -> bool {
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated string and `fs` is a writable
    // `statfs`; on success the kernel has initialised it.
    let ok = unsafe { libc::statfs(c_path.as_ptr(), fs.as_mut_ptr()) } == 0;
    // A path that can't be reached isn't a boundary the walk needs to stop at.
    ok && unsafe { mount_point(&fs.assume_init()) }.is_some_and(|m| m == path)
}

/// Identity of a directory, for recognising the same folder reached twice.
pub type Fingerprint = (u64, u64);

/// Fingerprint of a single path, or `None` if it can't be read.
pub fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let meta = fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// How a directory inside [`DATA_VOLUME`] relates to the path of the same name
/// under `/` — its *mirror*, the place a firmlink would make it appear.
///
/// Firmlinks nest: `/Users` is one, and so is `/System/Library/Caches` while
/// `/System` itself is an ordinary directory on the system volume. So a walk
/// cannot decide this at the top of the data volume; it asks folder by folder
/// on the way down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirrored {
    /// The same directory under a second name: whatever counts it at `/`
    /// already counted it, and counting it here doubles it.
    Same,
    /// A different directory that happens to share a name. Both are real, both
    /// count, and its children may still be mirrored.
    Different,
    /// Nothing at that path. Nothing below it can be firmlinked either, since a
    /// firmlink is reachable only through a parent that exists at `/`.
    Absent,
}

/// Compare a directory with the path it would occupy at `/`.
pub fn mirrored(path: &Path, mirror: &Path) -> Mirrored {
    let Some(other) = fingerprint(mirror) else {
        return Mirrored::Absent;
    };
    if fingerprint(path) == Some(other) {
        Mirrored::Same
    } else {
        Mirrored::Different
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_filesystem_is_always_a_mount_point() {
        let mounts = Mounts::read();
        assert!(!mounts.is_empty(), "the mount table must be readable");
        assert!(mounts.is_mount_point(Path::new("/")));
    }

    #[test]
    fn the_table_and_the_per_path_fallback_agree() {
        let table = Mounts::read();
        // An empty `Mounts` is the fallback path: no table, one `statfs` per
        // question. It has to answer the same way, or a sandbox that withholds
        // the table would quietly bring back the boundary-crossing walk.
        let fallback = Mounts::default();
        for path in ["/", "/System/Volumes/Data", "/Users", "/usr/bin"] {
            let path = Path::new(path);
            if !path.exists() {
                continue;
            }
            assert_eq!(
                table.is_mount_point(path),
                fallback.is_mount_point(path),
                "{} is read the same way with and without the table",
                path.display()
            );
        }
    }

    #[test]
    fn firmlinked_directories_are_not_mount_points() {
        let mounts = Mounts::read();
        // `/Users` lives on the data volume but is reached through a firmlink,
        // so it is a plain directory as far as the mount table is concerned —
        // the distinction the whole module exists to make.
        assert!(!mounts.is_mount_point(Path::new("/Users")));
        assert!(!mounts.is_mount_point(Path::new("/Users/nonexistent-xyz")));
    }

    #[test]
    fn the_data_volume_is_a_mount_point_whose_folders_mirror_root() {
        let data = Path::new(DATA_VOLUME);
        if !data.exists() {
            return; // Pre-Catalina layout: nothing to assert.
        }
        assert!(Mounts::read().is_mount_point(data));

        // The same directory under two names: this is the double count.
        assert_eq!(
            mirrored(&data.join("Users"), Path::new("/Users")),
            Mirrored::Same,
            "/System/Volumes/Data/Users is the folder /Users already leads to"
        );
        // And the nested case, which is why the answer can't be decided from
        // the top level alone: `System` is two different directories, but the
        // `Library/Caches` inside it is one.
        let caches = data.join("System/Library/Caches");
        if caches.exists() {
            assert_eq!(
                mirrored(&data.join("System"), Path::new("/System")),
                Mirrored::Different
            );
            assert_eq!(
                mirrored(&caches, Path::new("/System/Library/Caches")),
                Mirrored::Same
            );
        }
        assert_eq!(
            mirrored(&data.join("MobileSoftwareUpdate"), Path::new("/MobileSoftwareUpdate")),
            Mirrored::Absent,
            "what has no counterpart at / is the data volume's own"
        );
    }
}
