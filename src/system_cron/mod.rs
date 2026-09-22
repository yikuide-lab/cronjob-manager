pub mod cron_d;
pub mod crontab;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::Result;

/// Atomically replace `path` with `contents`.
///
/// Writes to a sibling dot-prefixed temp file first (so cron.d tooling
/// ignores it), syncs it, then renames over the target — a crash mid-write
/// can never truncate the target. Permissions of an existing file are
/// preserved; new files get 0644.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = temp_path(path);
    let mode = fs::metadata(path)
        .map(|m| m.permissions().mode())
        .unwrap_or(0o644);

    // Clear any stale temp file from a previous crashed run.
    let _ = fs::remove_file(&tmp);

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    drop(file);

    fs::rename(&tmp, path)?;
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "crontab".to_string());
    dir.join(format!(".{name}.tmp{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_content_and_preserves_mode() {
        let dir = std::env::temp_dir().join(format!("cronjob-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("crontab");
        fs::write(&path, "old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&path, "new\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // No leftover temp files.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_atomic_creates_new_file_0644() {
        let dir = std::env::temp_dir().join(format!("cronjob-test-new-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fresh");
        write_atomic(&path, "x\n").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
