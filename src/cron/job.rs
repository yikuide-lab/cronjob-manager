use std::fmt;

/// Identifies where a cron job comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CronSource {
    /// The current user's crontab.
    User,
    /// `/etc/crontab`.
    SystemCrontab,
    /// A file under `/etc/cron.d/`.
    CronD { file: String },
}

impl CronSource {
    /// Human-readable source name.
    pub fn display_name(&self) -> String {
        match self {
            CronSource::User => "User".to_string(),
            CronSource::SystemCrontab => "/etc/crontab".to_string(),
            CronSource::CronD { file } => format!("/etc/cron.d/{}", file),
        }
    }

    /// Whether this source is system-level and needs root to modify.
    pub fn is_system(&self) -> bool {
        !matches!(self, CronSource::User)
    }
}

impl fmt::Display for CronSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// A single cron job extracted from a crontab file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronJob {
    /// Stable UI identifier.
    pub id: String,
    /// Where this job was loaded from.
    pub source: CronSource,
    /// Whether the job is currently enabled (not commented out).
    pub enabled: bool,
    /// Cron schedule expression, e.g. `0 * * * *`.
    pub schedule: String,
    /// The user under which the command runs. Only present for system/cron.d entries.
    pub user: Option<String>,
    /// The command to execute.
    pub command: String,
}

impl CronJob {
    pub fn new(
        id: String,
        source: CronSource,
        enabled: bool,
        schedule: String,
        user: Option<String>,
        command: String,
    ) -> Self {
        Self {
            id,
            source,
            enabled,
            schedule,
            user,
            command,
        }
    }
}

/// A parsed crontab file, preserving non-job lines so it can be written back intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrontabLine {
    Job(CronJob),
    Env(String),
    Comment(String),
    Blank,
}

/// Parsed representation of a crontab-like file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrontabFile {
    pub source: CronSource,
    pub lines: Vec<CrontabLine>,
}

impl CrontabFile {
    pub fn new(source: CronSource) -> Self {
        Self {
            source,
            lines: Vec::new(),
        }
    }

    /// Iterate over all job entries.
    pub fn jobs(&self) -> impl Iterator<Item = &CronJob> {
        self.lines.iter().filter_map(|line| match line {
            CrontabLine::Job(job) => Some(job),
            _ => None,
        })
    }

    /// Mutable access to all job entries.
    pub fn jobs_mut(&mut self) -> impl Iterator<Item = &mut CronJob> {
        self.lines.iter_mut().filter_map(|line| match line {
            CrontabLine::Job(job) => Some(job),
            _ => None,
        })
    }

    /// Find a job by id.
    pub fn find_job(&self, id: &str) -> Option<&CronJob> {
        self.jobs().find(|job| job.id == id)
    }

    /// Find a job by id (mutable).
    pub fn find_job_mut(&mut self, id: &str) -> Option<&mut CronJob> {
        self.jobs_mut().find(|job| job.id == id)
    }

    /// Remove a job by id.
    pub fn remove_job(&mut self, id: &str) -> bool {
        let mut removed = false;
        self.lines.retain(|line| match line {
            CrontabLine::Job(job) if job.id == id => {
                removed = true;
                false
            }
            _ => true,
        });
        removed
    }

    /// Append a new job.
    pub fn add_job(&mut self, job: CronJob) {
        self.lines.push(CrontabLine::Job(job));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_job_keeps_other_lines() {
        let mut file = CrontabFile::new(CronSource::User);
        let job_a = CronJob::new(
            "a".into(),
            CronSource::User,
            true,
            "* * * * *".into(),
            None,
            "echo a".into(),
        );
        let job_b = CronJob::new(
            "b".into(),
            CronSource::User,
            true,
            "* * * * *".into(),
            None,
            "echo b".into(),
        );
        file.add_job(job_a);
        file.lines.push(CrontabLine::Comment("# hi".into()));
        file.add_job(job_b);

        assert!(file.remove_job("a"));
        assert_eq!(file.jobs().count(), 1);
        assert!(matches!(file.lines[0], CrontabLine::Comment(_)));
        assert!(!file.remove_job("missing"));
    }
}
