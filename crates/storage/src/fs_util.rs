//! Small filesystem helpers: atomic writes and owner-only permissions.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Who may read a written file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Default permissions (umask).
    Normal,
    /// Owner read/write only. On Unix the file is created with mode 0600. On Windows the file
    /// inherits the ACL of the user profile directory it lives in, which already excludes other
    /// users, so nothing extra is done.
    Private,
}

/// Writes `bytes` to `path` atomically: a sibling temp file is written, flushed and renamed over
/// the target, so a crash never leaves a half-written file. Parent directories are created.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], access: Access) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let tmp = tmp_path(path);
    let result = write_new(&tmp, bytes, access).and_then(|()| {
        fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
        // The rename keeps the temp file's mode, but tighten again in case the target existed
        // with looser permissions on a filesystem where rename did not replace the inode.
        if access == Access::Private {
            restrict(path)?;
        }
        Ok(())
    });
    if result.is_err() {
        // Best effort: do not leave temp files (possibly holding secrets) behind.
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp{}", std::process::id()));
    path.with_file_name(name)
}

fn write_new(path: &Path, bytes: &[u8], access: Access) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if access == Access::Private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file: File = opts.open(path).map_err(|e| Error::io(path, e))?;
    if access == Access::Private {
        // `mode` only applies when the file is created. A stale temp file may already exist.
        restrict(path)?;
    }
    file.write_all(bytes).map_err(|e| Error::io(path, e))?;
    file.sync_all().map_err(|e| Error::io(path, e))?;
    Ok(())
}

/// Makes `path` readable and writable by its owner only.
#[cfg(unix)]
pub(crate) fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| Error::io(path, e))
}

/// Windows: files under the user profile inherit an ACL that grants access to the user, SYSTEM
/// and Administrators only, so there is nothing to tighten.
#[cfg(not(unix))]
pub(crate) fn restrict(_path: &Path) -> Result<()> {
    Ok(())
}

/// Removes a file. A file that does not exist counts as removed.
pub(crate) fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Reads a file. `Ok(None)` if it does not exist.
pub(crate) fn read_if_exists(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Moves a corrupt file aside to `<name>.corrupt` (replacing an older one) so the user can
/// inspect it and the next save does not silently destroy it. Returns the new path.
pub(crate) fn quarantine(path: &Path) -> Result<PathBuf> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".corrupt");
    let target = path.with_file_name(name);
    remove_if_exists(&target)?;
    fs::rename(path, &target).map_err(|e| Error::io(path, e))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parents_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/file.txt");
        write_atomic(&path, b"one", Access::Normal).unwrap();
        write_atomic(&path, b"two", Access::Normal).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        // No temp file left behind.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&path, b"new", Access::Private).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn quarantine_moves_file_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, b"junk").unwrap();
        let moved = quarantine(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(moved).unwrap(), b"junk");
    }
}
