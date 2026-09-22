use std::sync::atomic::{AtomicU64, Ordering};

use crate::cron::job::{CronJob, CronSource, CrontabFile, CrontabLine};
use crate::cron::schedule::CronSchedule;
use crate::error::{AppError, Result};

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate a new unique job id.
pub fn next_job_id() -> String {
    format!("job-{}", ID_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Parse the contents of a crontab-like file.
///
/// `expect_user` should be `true` for system crontab and cron.d files,
/// where the 6th field is the user the command runs as. Anything that does
/// not parse as a job (comments, env lines, blanks, unknown lines) is kept
/// verbatim so the file round-trips without data loss.
pub fn parse_crontab(content: &str, source: CronSource, expect_user: bool) -> CrontabFile {
    let mut file = CrontabFile::new(source);
    for line in content.lines() {
        let parsed = match parse_line(line, file.source.clone(), expect_user) {
            Ok(parsed) => parsed,
            Err(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    CrontabLine::Blank
                } else if is_env_line(line) {
                    CrontabLine::Env(line.to_string())
                } else {
                    // Comments and anything unrecognized survive verbatim.
                    CrontabLine::Comment(line.to_string())
                }
            }
        };
        file.lines.push(parsed);
    }
    file
}

fn parse_line(line: &str, source: CronSource, expect_user: bool) -> Result<CrontabLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(AppError::Parse("empty line".into()));
    }

    let (enabled, body) = if let Some(after_hash) = trimmed.strip_prefix('#') {
        let after = after_hash.trim_start();
        // Only treat a comment as a disabled job when what follows actually
        // parses as one; plain comments stay comments.
        if body_looks_like_job(after) {
            (false, after)
        } else {
            return Err(AppError::Parse("comment".into()));
        }
    } else {
        (true, trimmed)
    };

    if is_env_line(body) {
        return Err(AppError::Parse("environment variable".into()));
    }

    let job = parse_job_body(body, source, enabled, expect_user)
        .map_err(|_| AppError::Parse("not a job line".into()))?;
    Ok(CrontabLine::Job(job))
}

/// Build a job from a line body (no leading `#`).
fn parse_job_body(
    body: &str,
    source: CronSource,
    enabled: bool,
    expect_user: bool,
) -> Result<CronJob> {
    let first_token = body.split_whitespace().next().unwrap_or("");
    if first_token.starts_with('@') {
        // Nickname schedule (@daily, @reboot, ...): one field instead of five.
        let head_count = if expect_user { 2 } else { 1 };
        let (fields, command) = split_head_preserving(body, head_count);
        if fields.len() < head_count || command.is_empty() {
            return Err(AppError::Parse(
                "missing fields after schedule nickname".into(),
            ));
        }
        let schedule = fields[0].to_string();
        CronSchedule::parse(&schedule)?;
        let user = expect_user.then(|| fields[1].to_string());
        return Ok(CronJob::new(
            next_job_id(),
            source,
            enabled,
            schedule,
            user,
            command.trim_end().to_string(),
        ));
    }

    // 5 schedule fields (+ user for system files); the command is kept as the
    // original line remainder so internal spacing survives the round-trip.
    let head_count = if expect_user { 6 } else { 5 };
    let (fields, command) = split_head_preserving(body, head_count);
    if fields.len() < head_count || command.is_empty() {
        return Err(AppError::Parse(format!(
            "expected at least {} fields, got {}",
            head_count + 1,
            fields.len()
        )));
    }

    let schedule = fields[..5].join(" ");
    CronSchedule::parse(&schedule)?;
    let user = expect_user.then(|| fields[5].to_string());

    Ok(CronJob::new(
        next_job_id(),
        source,
        enabled,
        schedule,
        user,
        command.trim_end().to_string(),
    ))
}

/// Split off the first `count` whitespace-separated fields and return them
/// together with the rest of the line, whose internal spacing is preserved
/// verbatim (byte indexing is safe: field boundaries are ASCII whitespace).
fn split_head_preserving(body: &str, count: usize) -> (Vec<&str>, &str) {
    let mut fields = Vec::with_capacity(count);
    let bytes = body.as_bytes();
    let mut idx = 0;
    for _ in 0..count {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let start = idx;
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if start == idx {
            break; // ran out of fields
        }
        fields.push(&body[start..idx]);
    }
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    (fields, &body[idx..])
}

/// Whether a stripped line body plausibly parses as a job. Used to decide
/// whether a `#`-prefixed line is a disabled job or an ordinary comment.
fn body_looks_like_job(body: &str) -> bool {
    let mut fields = body.split_whitespace();
    let Some(first) = fields.next() else {
        return false;
    };
    // Nickname schedules (@daily, @reboot, ...) stand alone.
    if first.starts_with('@') {
        return CronSchedule::parse(first).is_ok();
    }
    // The first field must be numeric/wildcard-ish; in particular a month or
    // weekday name ("JAN", "MON") is also valid here.
    if !first
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '*' | '-' | ',' | '/'))
        && !is_schedule_name(first)
    {
        return false;
    }
    // The next four fields must form a valid schedule.
    let rest: Vec<&str> = fields.take(4).collect();
    if rest.len() < 4 {
        return false;
    }
    let candidate = format!("{} {}", first, rest.join(" "));
    CronSchedule::parse(&candidate).is_ok()
}

fn is_schedule_name(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ]
    .contains(&upper.as_str())
        || ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"].contains(&upper.as_str())
}

fn is_env_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return false;
    }
    if let Some(eq_pos) = trimmed.find('=') {
        let key = &trimmed[..eq_pos];
        // Environment assignments have no whitespace before the `=` and do
        // not start with a schedule-like field.
        if !key.contains(|c: char| c.is_whitespace()) && !body_looks_like_job(trimmed) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_crontab() {
        let text =
            "# comment\nSHELL=/bin/bash\n\n0 * * * * echo hello\n# 5 * * * * echo disabled\n";
        let file = parse_crontab(text, CronSource::User, false);
        assert_eq!(file.jobs().count(), 2);

        let jobs: Vec<_> = file.jobs().collect();
        assert!(jobs[0].enabled);
        assert_eq!(jobs[0].schedule, "0 * * * *");
        assert_eq!(jobs[0].command, "echo hello");
        assert!(!jobs[1].enabled);
        assert_eq!(jobs[1].command, "echo disabled");
    }

    #[test]
    fn parse_system_crontab() {
        let text = "0 * * * * root /usr/bin/foo\n";
        let file = parse_crontab(text, CronSource::SystemCrontab, true);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].user.as_deref(), Some("root"));
        assert_eq!(jobs[0].command, "/usr/bin/foo");
    }

    #[test]
    fn disabled_job_with_name_fields_survives_reload() {
        // Regression: `# 0 0 1 JAN * cmd` and `# * * * * MON cmd` used to be
        // classified as plain comments and disappear from the job list.
        let text = "0 0 1 JAN * root /usr/bin/a\n# 0 12 * * MON root /usr/bin/b\n";
        let file = parse_crontab(text, CronSource::SystemCrontab, true);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 2, "both jobs must be recognized");
        assert!(jobs[0].enabled);
        assert!(!jobs[1].enabled);
        assert_eq!(jobs[1].schedule, "0 12 * * MON");
        assert_eq!(jobs[1].user.as_deref(), Some("root"));
        assert_eq!(jobs[1].command, "/usr/bin/b");
    }

    #[test]
    fn system_line_without_command_rejected() {
        // Regression: a 6-field system line used to parse with an empty command.
        let file = parse_crontab("0 * * * * root\n", CronSource::SystemCrontab, true);
        assert_eq!(file.jobs().count(), 0);
        // The unparsable line is preserved verbatim.
        assert!(matches!(file.lines[0], CrontabLine::Comment(_)));
    }

    #[test]
    fn user_line_needs_command() {
        let file = parse_crontab("* * * * *\n", CronSource::User, false);
        assert_eq!(file.jobs().count(), 0);
    }

    #[test]
    fn comment_without_space_after_hash() {
        let file = parse_crontab("#30 4 * * * /usr/bin/backup\n", CronSource::User, false);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert!(!jobs[0].enabled);
    }

    #[test]
    fn prose_comment_stays_comment() {
        let file = parse_crontab("# run the backup every day\n", CronSource::User, false);
        assert_eq!(file.jobs().count(), 0);
        assert!(matches!(file.lines[0], CrontabLine::Comment(_)));
    }

    #[test]
    fn env_line_detection() {
        assert!(is_env_line("SHELL=/bin/bash"));
        assert!(is_env_line("PATH=/usr/bin:/bin"));
        assert!(!is_env_line("0 * * * * echo hi=there"));
        assert!(!is_env_line("# SHELL=/bin/bash"));
        assert!(!is_env_line("BAD LINE=x"));
    }

    #[test]
    fn at_nickname_jobs_are_parsed() {
        let text = "@reboot /usr/bin/init-thing\n@daily /usr/bin/daily.sh\n";
        let file = parse_crontab(text, CronSource::User, false);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].schedule, "@reboot");
        assert_eq!(jobs[0].command, "/usr/bin/init-thing");
        assert_eq!(jobs[1].schedule, "@daily");
        assert_eq!(jobs[1].user, None);
        // Round-trips unchanged.
        assert_eq!(crate::cron::serialize_file(&file), text);
    }

    #[test]
    fn at_nickname_system_entry_with_user() {
        let file = parse_crontab("@daily root /usr/bin/x\n", CronSource::SystemCrontab, true);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].user.as_deref(), Some("root"));
        assert_eq!(jobs[0].command, "/usr/bin/x");
    }

    #[test]
    fn disabled_at_nickname_is_a_job() {
        let file = parse_crontab("# @reboot /usr/bin/x\n", CronSource::User, false);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert!(!jobs[0].enabled);
        assert_eq!(jobs[0].schedule, "@reboot");
    }

    #[test]
    fn at_nickname_without_command_stays_verbatim() {
        let file = parse_crontab("@daily\n", CronSource::User, false);
        assert_eq!(file.jobs().count(), 0);
        assert!(matches!(file.lines[0], CrontabLine::Comment(_)));
    }

    #[test]
    fn unknown_nickname_stays_verbatim() {
        let file = parse_crontab("@fortnightly /usr/bin/x\n", CronSource::User, false);
        assert_eq!(file.jobs().count(), 0);
    }

    #[test]
    fn command_internal_whitespace_is_preserved() {
        // Regression: `split_whitespace().join(" ")` used to collapse runs of
        // spaces inside commands when any edit rewrote the file.
        let text = "0 * * * * echo a   b\t c\n";
        let file = parse_crontab(text, CronSource::User, false);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].command, "echo a   b\t c");
        assert_eq!(crate::cron::serialize_file(&file), text);
    }

    #[test]
    fn command_spacing_preserved_for_system_files() {
        let text = "0 * * * * root printf  \"%s\"   arg\n";
        let file = parse_crontab(text, CronSource::SystemCrontab, true);
        let jobs: Vec<_> = file.jobs().collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].command, "printf  \"%s\"   arg");
        assert_eq!(crate::cron::serialize_file(&file), text);
    }
}
