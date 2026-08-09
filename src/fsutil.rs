use miette::{miette, Result};
use std::io::Write;
use std::path::Path;

/// Write bytes atomically: temp file in the target directory, fsync, rename,
/// fsync the directory. Preserves permissions of an existing target and
/// creates the parent directory when missing.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| miette!("create {}: {error}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| miette!("create temporary output: {error}"))?;
    if let Ok(metadata) = std::fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|error| miette!("preserve {} permissions: {error}", path.display()))?;
    }
    temporary
        .write_all(contents)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| miette!("write {}: {error}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| miette!("replace {}: {}", path.display(), error.error))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| miette!("sync output directory {}: {error}", parent.display()))?;
    Ok(())
}
