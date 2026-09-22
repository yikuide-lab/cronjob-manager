use std::io::{self, Write};
use std::process::{Command, Output, Stdio};

use crate::cron::{CronSource, CrontabFile, parse_crontab};
use crate::error::{AppError, Result};

/// How the app talks to a `crontab`-like tool. Production shells out to the
/// real `crontab`; tests substitute a fake so no real crontab is touched.
trait CrontabIo {
    /// Equivalent of `crontab -l`.
    fn list(&self) -> io::Result<Output>;
    /// Equivalent of `crontab -`: install `text` as the new crontab.
    fn install(&self, text: &str) -> io::Result<Output>;
}

/// Production backend: the system `crontab` command.
struct SystemCrontab;

impl CrontabIo for SystemCrontab {
    fn list(&self) -> io::Result<Output> {
        Command::new("crontab").arg("-l").output()
    }

    fn install(&self, text: &str) -> io::Result<Output> {
        let mut child = Command::new("crontab")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("failed to open crontab stdin"))?;
            // crontab reads all of stdin before producing any output, so
            // this cannot deadlock on the stderr pipe.
            stdin.write_all(text.as_bytes())?;
        }
        child.wait_with_output()
    }
}

/// Read the current user's crontab.
pub fn read() -> Result<CrontabFile> {
    read_with(&SystemCrontab)
}

/// Write the current user's crontab.
pub fn write(file: &CrontabFile) -> Result<()> {
    write_with(&SystemCrontab, file)
}

/// Core read logic, parameterized over the crontab backend.
fn read_with(io: &impl CrontabIo) -> Result<CrontabFile> {
    let output = io.list()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.to_ascii_lowercase().contains("no crontab") {
            return Ok(CrontabFile::new(CronSource::User));
        }
        return Err(AppError::CommandFailed(format!(
            "crontab -l failed: {}",
            stderr.trim()
        )));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_crontab(&text, CronSource::User, false))
}

/// Core write logic, parameterized over the crontab backend.
fn write_with(io: &impl CrontabIo, file: &CrontabFile) -> Result<()> {
    let text = crate::cron::serialize_file(file);
    let output = io.install(&text)?;
    if !output.status.success() {
        return Err(AppError::CommandFailed(format!(
            "crontab - failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::job::CronJob;
    use crate::cron::{next_job_id, serialize_file};
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Mutex;

    fn ok_output(stdout: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn failed_output(stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(0x0100), // exit code 1
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// Fake backend with canned responses; records the installed text.
    struct FakeCrontab {
        listing: Output,
        install_result: Output,
        installed: Mutex<Vec<String>>,
    }

    impl CrontabIo for FakeCrontab {
        fn list(&self) -> io::Result<Output> {
            Ok(self.listing.clone())
        }

        fn install(&self, text: &str) -> io::Result<Output> {
            self.installed.lock().unwrap().push(text.to_string());
            Ok(self.install_result.clone())
        }
    }

    fn fake(listing: Output, install_result: Output) -> FakeCrontab {
        FakeCrontab {
            listing,
            install_result,
            installed: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn read_parses_listing() {
        let io = fake(
            ok_output("SHELL=/bin/bash\n0 * * * * echo hello\n"),
            ok_output(""),
        );
        let file = read_with(&io).unwrap();
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].command, "echo hello");
    }

    #[test]
    fn read_treats_no_crontab_as_empty() {
        let io = fake(failed_output("no crontab for somebody\n"), ok_output(""));
        let file = read_with(&io).unwrap();
        assert_eq!(file.jobs().count(), 0);
    }

    #[test]
    fn read_surfaces_command_failure() {
        let io = fake(
            failed_output("crontab: daemon unreachable\n"),
            ok_output(""),
        );
        let err = read_with(&io).unwrap_err();
        assert!(err.to_string().contains("daemon unreachable"), "{err}");
    }

    #[test]
    fn write_installs_serialized_text() {
        let io = fake(ok_output(""), ok_output(""));
        let mut file = CrontabFile::new(CronSource::User);
        file.add_job(CronJob::new(
            next_job_id(),
            CronSource::User,
            true,
            "0 9 * * 1".to_string(),
            None,
            "/usr/bin/weekly".to_string(),
        ));

        write_with(&io, &file).unwrap();
        let installed = io.installed.lock().unwrap();
        assert_eq!(installed.as_slice(), [serialize_file(&file)]);
    }

    #[test]
    fn write_surfaces_command_failure() {
        let io = fake(ok_output(""), failed_output("crontab: cannot install\n"));
        let err = write_with(&io, &CrontabFile::new(CronSource::User)).unwrap_err();
        assert!(err.to_string().contains("cannot install"), "{err}");
    }
}
