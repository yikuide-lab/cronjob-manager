use crate::cron::job::{CronJob, CrontabFile, CrontabLine};

/// Serialize a single job to a crontab line.
pub fn serialize_job(job: &CronJob) -> String {
    let line = match &job.user {
        Some(user) => format!("{} {} {}", job.schedule, user, job.command),
        None => format!("{} {}", job.schedule, job.command),
    };
    if job.enabled {
        line
    } else {
        format!("# {line}")
    }
}

/// Serialize a whole crontab file back to text.
pub fn serialize_file(file: &CrontabFile) -> String {
    let mut out = String::new();
    for line in &file.lines {
        let text = match line {
            CrontabLine::Job(job) => serialize_job(job),
            CrontabLine::Env(s) | CrontabLine::Comment(s) => s.clone(),
            CrontabLine::Blank => String::new(),
        };
        out.push_str(&text);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::job::CronSource;
    use crate::cron::parser::parse_crontab;

    #[test]
    fn round_trip_user_crontab() {
        let text = "SHELL=/bin/bash\n\n0 * * * * echo hello\n# 5 * * * * echo disabled\n";
        let file = parse_crontab(text, CronSource::User, false);
        let serialized = serialize_file(&file);
        assert_eq!(serialized, text);
    }

    #[test]
    fn toggling_disabled_named_schedule_round_trips() {
        let text = "# 0 12 * * MON /usr/bin/b\n";
        let mut file = parse_crontab(text, CronSource::User, false);
        assert_eq!(file.jobs().count(), 1);

        // Toggle on, serialize, re-parse: the job must still be a job.
        let job_id = file.jobs().next().unwrap().id.clone();
        file.find_job_mut(&job_id).unwrap().enabled = true;
        let serialized = serialize_file(&file);
        assert_eq!(serialized, "0 12 * * MON /usr/bin/b\n");
        let reparsed = parse_crontab(&serialized, CronSource::User, false);
        assert_eq!(reparsed.jobs().count(), 1);
        assert!(reparsed.jobs().next().unwrap().enabled);
    }
}
