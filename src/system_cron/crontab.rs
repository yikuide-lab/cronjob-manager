use std::fs;
use std::path::Path;

use crate::cron::{CronSource, CrontabFile, parse_crontab};
use crate::error::Result;

use super::write_atomic;

const SYSTEM_CRONTAB_PATH: &str = "/etc/crontab";

/// Read the system `/etc/crontab` file.
pub fn read() -> Result<CrontabFile> {
    read_path(Path::new(SYSTEM_CRONTAB_PATH))
}

/// Atomically write the system `/etc/crontab` file.
pub fn write(file: &CrontabFile) -> Result<()> {
    write_path(Path::new(SYSTEM_CRONTAB_PATH), file)
}

/// `read` against an explicit path (test seam).
pub(crate) fn read_path(path: &Path) -> Result<CrontabFile> {
    let text = fs::read_to_string(path)?;
    Ok(parse_crontab(&text, CronSource::SystemCrontab, true))
}

/// `write` against an explicit path (test seam).
pub(crate) fn write_path(path: &Path, file: &CrontabFile) -> Result<()> {
    let text = crate::cron::serialize_file(file);
    write_atomic(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::job::CronJob;
    use crate::cron::next_job_id;
    use std::path::PathBuf;

    fn temp_file(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cronjob-syscrontab-{tag}-{}", std::process::id()))
    }

    #[test]
    fn read_write_round_trip() {
        let path = temp_file("rt");
        let _ = std::fs::remove_file(&path);

        let mut file = CrontabFile::new(CronSource::SystemCrontab);
        file.lines
            .push(crate::cron::job::CrontabLine::Env("SHELL=/bin/sh".into()));
        file.add_job(CronJob::new(
            next_job_id(),
            CronSource::SystemCrontab,
            true,
            "17 * * * *".into(),
            Some("root".into()),
            "run-parts /etc/cron.hourly".into(),
        ));

        write_path(&path, &file).unwrap();
        let reloaded = read_path(&path).unwrap();
        assert_eq!(reloaded.source, CronSource::SystemCrontab);
        let jobs: Vec<_> = reloaded.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].user.as_deref(), Some("root"));
        assert_eq!(jobs[0].command, "run-parts /etc/cron.hourly");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn read_missing_file_is_error() {
        assert!(read_path(&temp_file("missing")).is_err());
    }
}
