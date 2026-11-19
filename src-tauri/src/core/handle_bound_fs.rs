//! No-clobber directory-entry moves anchored to stable parent directory FDs.
//!
//! This protects against an unrelated process replacing a *parent pathname*
//! after preflight: we never pass mutable absolute path strings to renameat.
//! It does not provide an atomic compare-and-rename of the *child inode*.
//! Callers must retain their own published/observed child identity guards.
//!
//! The first macOS/Linux implementation intentionally refuses any symlink in
//! the path to either parent, including intermediate ancestors. There is no
//! fallback to following symlinks or to stat-then-rename.

use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, Mode, OFlags, RenameFlags};

fn error(err: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(err.raw_os_error())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn parent_and_name(path: &Path) -> io::Result<(PathBuf, OsString)> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let name = match absolute.components().next_back() {
        Some(Component::Normal(name)) => name.to_os_string(),
        _ => {
            return Err(invalid(
                "Directory move requires an ordinary final path component",
            ))
        }
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| invalid("Directory move requires a parent"))?
        .to_path_buf();
    Ok((parent, name))
}

/// Walk from the root through each ancestor using handle-relative O_NOFOLLOW
/// and O_DIRECTORY. An unexpected symlink fails closed at the exact component.
fn open_parent_no_follow(parent: &Path) -> io::Result<OwnedFd> {
    if !parent.is_absolute() {
        return Err(invalid("Directory move parents must be absolute"));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = fs::openat(fs::CWD, "/", flags, Mode::empty()).map_err(error)?;
    for component in parent.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = fs::openat(&directory, name, flags, Mode::empty()).map_err(error)?;
            }
            _ => {
                return Err(invalid(
                    "Directory move parent contains a traversal component",
                ))
            }
        }
    }
    Ok(directory)
}

/// A quick stale-parent rejection on either side of the rename. The kernel
/// rename itself is anchored to FDs, so a parent path swap cannot redirect it
/// into an attacker-controlled directory after these checks.
///
/// This is not an atomic guarantee against a malicious actor swapping a
/// *source child name* between observation and rename.
fn parent_is_same_object(parent: &Path, bound: &OwnedFd) -> io::Result<bool> {
    let current = open_parent_no_follow(parent)?;
    let before = fs::fstat(bound).map_err(error)?;
    let after = fs::fstat(&current).map_err(error)?;
    Ok(before.st_dev == after.st_dev && before.st_ino == after.st_ino)
}

pub fn rename_directory_no_replace(source: &Path, target: &Path) -> io::Result<()> {
    rename_directory_no_replace_inner(source, target, || {})
}

/// Test seam: caller can rename a parent after its FD is bound, before the
/// no-clobber move. Production calls the same implementation with no hook.
#[cfg(test)]
pub(crate) fn rename_after_parents_open<F: FnOnce()>(
    source: &Path,
    target: &Path,
    hook: F,
) -> io::Result<()> {
    rename_directory_no_replace_inner(source, target, hook)
}

fn rename_directory_no_replace_inner<F: FnOnce()>(
    source: &Path,
    target: &Path,
    after_open: F,
) -> io::Result<()> {
    let (source_parent, source_name) = parent_and_name(source)?;
    let (target_parent, target_name) = parent_and_name(target)?;
    let from = open_parent_no_follow(&source_parent)?;
    let to = open_parent_no_follow(&target_parent)?;
    after_open();

    if !parent_is_same_object(&source_parent, &from)?
        || !parent_is_same_object(&target_parent, &to)?
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Directory parent changed after descriptor binding; no move performed",
        ));
    }

    fs::renameat_with(
        &from,
        source_name.as_os_str(),
        &to,
        target_name.as_os_str(),
        RenameFlags::NOREPLACE,
    )
    .map_err(error)?;

    // Source and destination parents may still be replaced immediately after
    // the system call. Never claim that an absolute pathname was stable for
    // the entire operation; the caller also checks the child identity.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;
    use tempfile::tempdir;

    #[test]
    fn refuses_to_follow_symlinked_intermediate_parent() {
        let tmp = tempdir().unwrap();
        // macOS /var is a system symlink to /private/var: exercise normal
        // canonical project roots without granting general symlink traversal.
        let root = stdfs::canonicalize(tmp.path()).unwrap();
        let real = root.join("real");
        stdfs::create_dir(&real).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        stdfs::create_dir(real.join("source")).unwrap();
        let err =
            rename_directory_no_replace(&link.join("source"), &real.join("target")).unwrap_err();
        assert!(err.raw_os_error().is_some(), "{err}");
        assert!(real.join("source").is_dir());
        assert!(!real.join("target").exists());
    }

    #[test]
    fn destination_parent_swap_after_open_cannot_redirect_or_touch_foreign_directory() {
        let tmp = tempdir().unwrap();
        // macOS /var is a system symlink to /private/var: exercise normal
        // canonical project roots without granting general symlink traversal.
        let root = stdfs::canonicalize(tmp.path()).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        let parked = root.join("parked-destination");
        stdfs::create_dir(&source).unwrap();
        stdfs::create_dir(&destination).unwrap();
        stdfs::create_dir(source.join("managed")).unwrap();
        stdfs::write(source.join("managed/SKILL.md"), "manager owned").unwrap();

        let err = rename_after_parents_open(
            &source.join("managed"),
            &destination.join("managed"),
            || {
                stdfs::rename(&destination, &parked).unwrap();
                stdfs::create_dir(&destination).unwrap();
                stdfs::create_dir(destination.join("managed")).unwrap();
                stdfs::write(destination.join("managed/SKILL.md"), "external content").unwrap();
            },
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
        assert_eq!(
            stdfs::read_to_string(source.join("managed/SKILL.md")).unwrap(),
            "manager owned"
        );
        assert_eq!(
            stdfs::read_to_string(destination.join("managed/SKILL.md")).unwrap(),
            "external content"
        );
        assert!(!parked.join("managed").exists());
    }

    #[test]
    fn source_parent_swap_after_open_cannot_move_replacement_with_equal_bytes() {
        let tmp = tempdir().unwrap();
        // macOS /var is a system symlink to /private/var: exercise normal
        // canonical project roots without granting general symlink traversal.
        let root = stdfs::canonicalize(tmp.path()).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        let parked = root.join("parked-source");
        stdfs::create_dir(&source).unwrap();
        stdfs::create_dir(&destination).unwrap();
        stdfs::create_dir(source.join("copy")).unwrap();
        stdfs::write(source.join("copy/SKILL.md"), "identical content").unwrap();

        let err =
            rename_after_parents_open(&source.join("copy"), &destination.join("copy"), || {
                stdfs::rename(&source, &parked).unwrap();
                stdfs::create_dir(&source).unwrap();
                stdfs::create_dir(source.join("copy")).unwrap();
                stdfs::write(source.join("copy/SKILL.md"), "identical content").unwrap();
            })
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
        assert_eq!(
            stdfs::read_to_string(parked.join("copy/SKILL.md")).unwrap(),
            "identical content"
        );
        assert_eq!(
            stdfs::read_to_string(source.join("copy/SKILL.md")).unwrap(),
            "identical content"
        );
        assert!(!destination.join("copy").exists());
    }

    #[test]
    fn external_process_swaps_parent_after_fd_binding_without_moving_foreign_copy() {
        let tmp = tempdir().unwrap();
        let root = stdfs::canonicalize(tmp.path()).unwrap();
        let from = root.join("source");
        let to = root.join("destination");
        let parked = root.join("parked-source");
        stdfs::create_dir(&from).unwrap();
        stdfs::create_dir(&to).unwrap();
        stdfs::create_dir(from.join("copy")).unwrap();
        stdfs::write(from.join("copy/SKILL.md"), "same committed bytes").unwrap();

        let result = rename_after_parents_open(&from.join("copy"), &to.join("copy"), || {
            // Actual second process performs the hostile rename and then
            // creates an equal-content replacement at the old pathname.
            let status = std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg("mv \"$1\" \"$2\" && mkdir \"$1\" && mkdir \"$1/copy\" && cp \"$2/copy/SKILL.md\" \"$1/copy/SKILL.md\"")
                    .arg("sh")
                    .arg(&from)
                    .arg(&parked)
                    .status()
                    .unwrap();
            assert!(status.success(), "hostile test child must swap the parent");
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            stdfs::read_to_string(from.join("copy/SKILL.md")).unwrap(),
            "same committed bytes"
        );
        assert_eq!(
            stdfs::read_to_string(parked.join("copy/SKILL.md")).unwrap(),
            "same committed bytes"
        );
        assert!(
            !to.join("copy").exists(),
            "no foreign or old Copy was moved"
        );
    }

    #[test]
    fn no_replace_move_succeeds_and_existing_destination_is_untouched() {
        let tmp = tempdir().unwrap();
        // macOS /var is a system symlink to /private/var: exercise normal
        // canonical project roots without granting general symlink traversal.
        let root = stdfs::canonicalize(tmp.path()).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        stdfs::create_dir(&source).unwrap();
        stdfs::create_dir(&destination).unwrap();
        stdfs::create_dir(source.join("copy")).unwrap();
        stdfs::write(source.join("copy/SKILL.md"), "manager").unwrap();
        rename_directory_no_replace(&source.join("copy"), &destination.join("copy")).unwrap();
        assert!(destination.join("copy/SKILL.md").is_file());
        stdfs::create_dir(source.join("copy")).unwrap();
        stdfs::write(source.join("copy/SKILL.md"), "external").unwrap();
        assert!(
            rename_directory_no_replace(&source.join("copy"), &destination.join("copy")).is_err()
        );
        assert_eq!(
            stdfs::read_to_string(source.join("copy/SKILL.md")).unwrap(),
            "external"
        );
        assert_eq!(
            stdfs::read_to_string(destination.join("copy/SKILL.md")).unwrap(),
            "manager"
        );
    }
}
