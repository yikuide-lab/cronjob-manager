use chrono::{DateTime, Local, TimeZone};

use cronjob_manager::cron::job::{CronSource, CrontabLine};
use cronjob_manager::cron::{CronSchedule, parse_crontab, serialize_file, validate_schedule};

const SAMPLE_USER_CRONTAB: &str = r#"SHELL=/bin/bash
PATH=/usr/local/bin:/usr/bin

# Run backup every hour
0 * * * * /home/user/backup.sh

# Disabled maintenance job
# 30 2 * * 0 /home/user/cleanup.sh

*/5 9-17 * * 1-5 echo ping
"#;

const SAMPLE_SYSTEM_CRONTAB: &str = r#"SHELL=/bin/sh
PATH=/usr/local/sbin:/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin

17 * * * * root cd / && run-parts --report /etc/cron.hourly
25 6 * * * root test -x /usr/sbin/anacron || ( cd / && run-parts --report /etc/cron.daily )
"#;

#[test]
fn parse_and_serialize_user_crontab() {
    let file = parse_crontab(SAMPLE_USER_CRONTAB, CronSource::User, false);
    let jobs: Vec<_> = file.jobs().collect();
    assert_eq!(jobs.len(), 3);

    assert!(jobs[0].enabled);
    assert_eq!(jobs[0].schedule, "0 * * * *");
    assert_eq!(jobs[0].command, "/home/user/backup.sh");

    assert!(!jobs[1].enabled);
    assert_eq!(jobs[1].schedule, "30 2 * * 0");
    assert_eq!(jobs[1].command, "/home/user/cleanup.sh");

    assert_eq!(jobs[2].schedule, "*/5 9-17 * * 1-5");
    assert_eq!(jobs[2].command, "echo ping");

    // Non-job lines should be preserved.
    assert!(matches!(file.lines[0], CrontabLine::Env(_)));
    assert!(matches!(file.lines[3], CrontabLine::Comment(_)));

    let serialized = serialize_file(&file);
    assert_eq!(serialized, SAMPLE_USER_CRONTAB);
}

#[test]
fn parse_and_serialize_system_crontab() {
    let file = parse_crontab(SAMPLE_SYSTEM_CRONTAB, CronSource::SystemCrontab, true);
    let jobs: Vec<_> = file.jobs().collect();
    assert_eq!(jobs.len(), 2);

    assert_eq!(jobs[0].user.as_deref(), Some("root"));
    assert_eq!(jobs[0].schedule, "17 * * * *");
    assert_eq!(
        jobs[0].command,
        "cd / && run-parts --report /etc/cron.hourly"
    );

    assert_eq!(jobs[1].user.as_deref(), Some("root"));
    assert_eq!(jobs[1].schedule, "25 6 * * *");

    let serialized = serialize_file(&file);
    assert_eq!(serialized, SAMPLE_SYSTEM_CRONTAB);
}

#[test]
fn validate_various_schedules() {
    let valid = [
        "* * * * *",
        "0 0 * * 1",
        "*/10 9-17 * * MON-FRI",
        "0 0 1 JAN *",
        "0,30 8-20 * * *",
        "0 0 * * 7",
    ];
    for expr in &valid {
        assert!(validate_schedule(expr).is_ok(), "{expr} should be valid");
    }

    let invalid = [
        "* * * *",
        "60 * * * *",
        "0 0 0 * *",
        "* * * * * * *",
        "abc * * * *",
        "*/0 * * * *",
    ];
    for expr in &invalid {
        assert!(validate_schedule(expr).is_err(), "{expr} should be invalid");
    }
}

#[test]
fn disabled_named_schedule_round_trip() {
    // Regression: these used to be misread as plain comments and vanish.
    let text = "# 0 12 * * MON root /usr/bin/b\n# 0 0 1 JAN * root /usr/bin/a\n";
    let file = parse_crontab(text, CronSource::SystemCrontab, true);
    assert_eq!(file.jobs().count(), 2);
    let serialized = serialize_file(&file);
    assert_eq!(serialized, text);
}

#[test]
fn system_entry_without_command_is_rejected() {
    let file = parse_crontab("0 * * * * root\n", CronSource::SystemCrontab, true);
    assert_eq!(file.jobs().count(), 0);
    // Preserved verbatim instead of being rewritten as a broken job line.
    assert!(matches!(file.lines[0], CrontabLine::Comment(_)));
}

#[test]
fn schedule_preview_end_to_end() {
    let schedule = CronSchedule::parse("0 9 * * 1-5").unwrap();
    assert_eq!(schedule.describe(), "Every Monday to Friday at 09:00");

    // 2026-01-16 18:00 is a Friday; the next weekday run is Monday 09:00.
    let after: DateTime<Local> = Local.with_ymd_and_hms(2026, 1, 16, 18, 0, 0).unwrap();
    let next = schedule.next_after(&after).unwrap();
    let expected: DateTime<Local> = Local.with_ymd_and_hms(2026, 1, 19, 9, 0, 0).unwrap();
    assert_eq!(next, expected);
}
