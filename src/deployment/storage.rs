//! Private journal storage. Request files deliberately do not share its trust.
use fs2::FileExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};

const MAX_JSON: u64 = 128 * 1024;

/// Activation-lock failure. Only kernel lock contention is retryable; path,
/// ownership and descriptor errors need operator inspection.
#[derive(Debug)]
pub(super) struct LockFailure {
    pub busy: bool,
    pub message: String,
}

impl std::fmt::Display for LockFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for LockFailure {
    fn from(message: String) -> Self {
        Self {
            busy: false,
            message,
        }
    }
}

impl From<&str> for LockFailure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    unsafe { libc::geteuid() }
}

pub(super) fn absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "expected an absolute, non-traversing path: {}",
            path.display()
        ));
    }
    Ok(())
}

/// Root or this user must control every ancestor. Sticky root directories
/// permit private user state below /tmp without trusting another user's files.
pub(super) fn trusted_path(path: &Path) -> Result<(), String> {
    absolute(path)?;
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        current.push(part);
        let metadata = fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
        if metadata.uid() != 0 && metadata.uid() != uid() {
            return Err(format!("foreign owner on {}", current.display()));
        }
        if metadata.file_type().is_symlink() {
            let resolved = fs::canonicalize(&current).map_err(|error| error.to_string())?;
            trusted_path(&resolved)?;
        } else if metadata.mode() & 0o022 != 0
            && !(metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o1000 != 0)
        {
            return Err(format!("untrusted writable path: {}", current.display()));
        }
    }
    Ok(())
}

/// Open nonblocking and without following a final symlink: a malicious request
/// must not turn a privileged reader into a FIFO reader or unbounded allocation.
pub(super) fn read_json<T: DeserializeOwned>(path: &Path, trusted: bool) -> Result<T, String> {
    if trusted {
        trusted_path(path)?;
    }
    let resolved = if trusted {
        fs::canonicalize(path).map_err(|error| error.to_string())?
    } else {
        absolute(path)?;
        path.to_owned()
    };
    let file = open_without_links(&resolved)?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("JSON input must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_JSON + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_JSON {
        return Err("deployment JSON exceeds 128 KiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// Pin each directory descriptor before opening its child. Checking parents
/// with lstat followed by an ordinary open would race a request writer's rename.
fn open_without_links(path: &Path) -> Result<File, String> {
    let parts: Vec<_> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let mut directory = File::open("/").map_err(|error| error.to_string())?;
    for (index, name) in parts.iter().enumerate() {
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|error| error.to_string())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if index + 1 == parts.len() {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: the directory descriptor remains open and name is a live,
        // NUL-terminated single component. No creation mode is used.
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned a new, uniquely owned descriptor.
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

pub(super) fn private_dir(path: &Path) -> Result<(), String> {
    absolute(path)?;
    if !path.exists() {
        let parent = path.parent().ok_or("state directory has no parent")?;
        private_parents(parent)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|error| error.to_string())?;
    }
    trusted_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "state must be a private owned directory: {}",
            path.display()
        ));
    }
    Ok(())
}

fn private_parents(path: &Path) -> Result<(), String> {
    if path.exists() {
        trusted_path(path)
    } else {
        private_dir(path)
    }
}

pub(super) fn lock(path: &Path) -> Result<File, LockFailure> {
    absolute(path)?;
    trusted_path(path.parent().ok_or("lock has no parent")?)?;
    let create = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let file = match create {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| error.to_string())?,
        Err(error) => return Err(error.to_string().into()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || (metadata.uid() != 0 && metadata.uid() != uid())
    {
        return Err("activation lock is not an owned regular anchor".into());
    }
    file.try_lock_exclusive().map_err(|error| LockFailure {
        busy: error.kind() == std::io::ErrorKind::WouldBlock,
        message: format!("deployment lock {}: {error}", path.display()),
    })?;
    // Keep this open-file-description lock alive in action descendants if the
    // worker is killed, so recovery cannot overlap a surviving activator.
    // SAFETY: file owns this descriptor; only its close-on-exec flag changes.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
    {
        return Err(std::io::Error::last_os_error().to_string().into());
    }
    Ok(file)
}

pub(super) fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    private_dir(path.parent().ok_or("journal has no parent")?)?;
    if fs::symlink_metadata(path).is_ok() {
        trusted_path(path)?;
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != uid()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err("journal must be a private owned regular file".into());
        }
    }
    crate::fsutil::atomic_write(
        path,
        &serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn untrusted_requests_are_bounded_regular_files_not_links() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("request");
        fs::write(&source, "{}").unwrap();
        assert!(read_json::<serde_json::Value>(&source, false).is_ok());
        let link = root.path().join("link");
        symlink(&source, &link).unwrap();
        assert!(read_json::<serde_json::Value>(&link, false).is_err());
        let parent_link = root.path().join("parent-link");
        symlink(root.path(), &parent_link).unwrap();
        assert!(read_json::<serde_json::Value>(&parent_link.join("request"), false).is_err());
        fs::write(&source, vec![b' '; MAX_JSON as usize + 1]).unwrap();
        assert!(read_json::<serde_json::Value>(&source, false).is_err());
        assert!(read_json::<serde_json::Value>(root.path(), false).is_err());
    }

    #[test]
    fn private_state_rejects_shared_or_symlinked_directories() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        private_dir(&state).unwrap();
        let link = root.path().join("alias");
        symlink(&state, &link).unwrap();
        assert!(private_dir(&link).is_err());
        fs::set_permissions(&state, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(private_dir(&state).is_err());
    }

    #[test]
    fn activation_uses_existing_kernel_lock_without_replacing_anchor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        let first = lock(&path).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        let busy = lock(&path).unwrap_err();
        assert!(busy.busy);
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!lock(&shared.join("lock")).unwrap_err().busy);
        drop(first);
        assert!(lock(&path).is_ok());
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    }
}
