use std::fs;
use std::path::Path;

use crate::cron::{CronSource, CrontabFile, parse_crontab};
use crate::error::{AppError, Result};

use super::write_atomic;

const CRON_D_DIR: &str = "/etc/cron.d";

/// Whether cron actually processes a file with this name in `/etc/cron.d`.
///
/// Per run-parts rules, only names of letters, digits, underscores and
/// hyphens are used; files with dots, backup suffixes (`~`, `.bak`) or a
/// leading dot are ignored by cron, so editing them has no effect.
pub fn is_managed_file_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Read every cron-managed file in `/etc/cron.d/`, sorted by file name.
pub fn read_all() -> Result<Vec<CrontabFile>> {
    read_all_in(Path::new(CRON_D_DIR))
}

/// Atomically write a single `/etc/cron.d/` file.
pub fn write(file: &CrontabFile) -> Result<()> {
    write_in(Path::new(CRON_D_DIR), file)
}

/// `read_all` against an explicit directory (test seam).
pub(crate) fn read_all_in(dir: &Path) -> Result<Vec<CrontabFile>> {
    // Collect name/text pairs first so the result can be sorted: read_dir
    // order is not stable, and a stable sidebar order avoids selection jumps.
    let mut entries: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| AppError::Other("invalid file name in /etc/cron.d".into()))?;
        if !is_managed_file_name(&name) {
            // Ignored by cron itself (dots, backups, dotfiles) — skip it.
            continue;
        }
        let text = fs::read_to_string(&path)?;
        entries.push((name, text));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(entries
        .into_iter()
        .map(|(name, text)| parse_crontab(&text, CronSource::CronD { file: name }, true))
        .collect())
}

/// `write` against an explicit directory (test seam).
pub(crate) fn write_in(dir: &Path, file: &CrontabFile) -> Result<()> {
    let CronSource::CronD { file: name } = &file.source else {
        return Err(AppError::Other("not a cron.d file".into()));
    };
    let path = dir.join(name);
    let text = crate::cron::serialize_file(file);
    write_atomic(&path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::job::{CronJob, CrontabLine};
    use crate::cron::next_job_id;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cronjob-crond-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn managed_file_names() {
        assert!(is_managed_file_name("backup"));
        assert!(is_managed_file_name("my-job_2"));
        assert!(!is_managed_file_name("e2scrub_all.rpmnew"));
        assert!(!is_managed_file_name(".hidden"));
        assert!(!is_managed_file_name("backup~"));
        assert!(!is_managed_file_name("backup.bak"));
        assert!(!is_managed_file_name(""));
    }

    #[test]
    fn read_filters_ignored_files_and_sorts() {
        let dir = temp_dir("filter");
        fs::write(dir.join("zeta"), "0 1 * * * root /bin/z\n").unwrap();
        fs::write(dir.join("alpha"), "0 2 * * * root /bin/a\n").unwrap();
        fs::write(dir.join("ignored.bak"), "garbage\n").unwrap();
        fs::write(dir.join(".hidden"), "garbage\n").unwrap();
        fs::write(dir.join("skip~"), "garbage\n").unwrap();

        let files = read_all_in(&dir).unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|f| match &f.source {
                CronSource::CronD { file } => file.as_str(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(names, ["alpha", "zeta"], "ignored files filtered, sorted");
        assert_eq!(files[0].jobs().next().unwrap().command, "/bin/a");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_creates_file_and_round_trips() {
        let dir = temp_dir("write");
        let mut file = CrontabFile::new(CronSource::CronD {
            file: "cronjob-manager-test".to_string(),
        });
        file.lines
            .push(CrontabLine::Comment("# managed by app".into()));
        file.add_job(CronJob::new(
            next_job_id(),
            file.source.clone(),
            false,
            "30 4 * * *".into(),
            Some("backup".into()),
            "/usr/local/bin/backup --quiet".into(),
        ));

        write_in(&dir, &file).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("cronjob-manager-test")).unwrap(),
            "# managed by app\n# 30 4 * * * backup /usr/local/bin/backup --quiet\n"
        );

        let reloaded = read_all_in(&dir).unwrap();
        assert_eq!(reloaded.len(), 1);
        let jobs: Vec<_> = reloaded[0].jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert!(!jobs[0].enabled);
        assert_eq!(jobs[0].user.as_deref(), Some("backup"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_rejects_non_cron_d_source() {
        let dir = temp_dir("reject");
        assert!(write_in(&dir, &CrontabFile::new(CronSource::User)).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
