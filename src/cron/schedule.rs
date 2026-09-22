//! Cron schedule model: parsing, validation, human-readable description and
//! next-run computation.
//!
//! This is the single source of truth for schedule semantics. The validator
//! delegates here, the UI uses `describe` for a friendly label and
//! `next_after` for the upcoming-run preview.

use chrono::{Datelike, Duration, Local, LocalResult, NaiveDate, TimeZone, Timelike};

use crate::error::{AppError, Result};

const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DOW_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DOW_FULL_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Upper bound for the next-run search: four years plus slack covers a
/// leap-day-only schedule (Feb 29) from any starting point.
const SEARCH_DAYS: i64 = 4 * 366 + 2;

/// A parsed 5-field cron expression. Each field is a bitmask over its value
/// range (bit `v - min` set means value `v` is allowed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    expr: String,
    minutes: u64,
    hours: u64,
    doms: u64,
    months: u64,
    dows: u64,
    dom_star: bool,
    dow_star: bool,
    /// `@reboot`: event-based, no time-based next run exists.
    reboot: bool,
}

impl CronSchedule {
    /// Parse a standard 5-field cron expression (minute hour dom month dow)
    /// or one of the `@`-nicknames from crontab(5) (`@daily`, `@reboot`, ...).
    pub fn parse(expr: &str) -> Result<Self> {
        let trimmed = expr.trim();
        if trimmed.starts_with('@') {
            return Self::parse_nickname(trimmed);
        }

        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(AppError::InvalidSchedule(
                expr.to_string(),
                format!("expected 5 fields, got {}", fields.len()),
            ));
        }

        let minutes = parse_field(fields[0], 0, 59, None, expr, "minute")?;
        let hours = parse_field(fields[1], 0, 23, None, expr, "hour")?;
        let doms = parse_field(fields[2], 1, 31, None, expr, "day of month")?;
        let months = parse_field(fields[3], 1, 12, Some((&MONTH_NAMES, 1)), expr, "month")?;
        // Day of week accepts 0-7 where both 0 and 7 mean Sunday; normalize to 0-6.
        let dows_raw = parse_field(fields[4], 0, 7, Some((&DOW_NAMES, 0)), expr, "day of week")?;
        let dows = normalize_dow(dows_raw);

        Ok(Self {
            expr: expr.to_string(),
            minutes,
            hours,
            doms,
            months,
            dows,
            dom_star: fields[2] == "*",
            dow_star: fields[4] == "*",
            reboot: false,
        })
    }

    /// Map an `@`-nickname onto its equivalent 5-field expression.
    /// Everything except `@reboot` becomes a plain time-based schedule, so
    /// describe/next_after work unchanged; `@reboot` is flagged as event-based.
    fn parse_nickname(expr: &str) -> Result<Self> {
        let (equiv, reboot) = match expr.to_ascii_lowercase().as_str() {
            "@yearly" | "@annually" => ("0 0 1 1 *", false),
            "@monthly" => ("0 0 1 * *", false),
            "@weekly" => ("0 0 * * 0", false),
            "@daily" | "@midnight" => ("0 0 * * *", false),
            "@hourly" => ("0 * * * *", false),
            "@reboot" => ("* * * * *", true),
            _ => {
                return Err(AppError::InvalidSchedule(
                    expr.to_string(),
                    format!("unknown schedule nickname '{expr}'"),
                ));
            }
        };
        let mut sched = Self::parse(equiv)?;
        sched.expr = expr.to_string();
        sched.reboot = reboot;
        Ok(sched)
    }

    /// Whether this is an `@reboot` schedule (runs once at every boot).
    pub fn is_reboot(&self) -> bool {
        self.reboot
    }

    /// Standard cron day semantics: when both day of month and day of week
    /// are restricted, a day matches if *either* field matches.
    fn day_matches(&self, date: NaiveDate) -> bool {
        let dom_ok = self.doms & (1 << (date.day() - 1)) != 0;
        let dow_ok = self.dows & (1 << date.weekday().num_days_from_sunday()) != 0;
        if self.dom_star {
            dow_ok
        } else if self.dow_star {
            dom_ok
        } else {
            dom_ok || dow_ok
        }
    }

    /// Find the next run strictly after `after`, at minute resolution.
    /// Returns `None` if no match occurs within the search horizon
    /// (e.g. `0 0 31 2 *` — Feb 31 never exists).
    pub fn next_after(&self, after: &chrono::DateTime<Local>) -> Option<chrono::DateTime<Local>> {
        if self.reboot {
            return None; // event-based, not predictable from the clock
        }
        // The first minute that could possibly match.
        let start = after
            .with_second(0)
            .and_then(|dt| dt.with_nanosecond(0))
            .and_then(|dt| dt.checked_add_signed(Duration::minutes(1)))?;

        let mut date = start.date_naive();
        for _ in 0..=SEARCH_DAYS {
            if self.months & (1 << (date.month() - 1)) == 0 {
                // Fast-forward to the first day of the next month.
                date = first_of_next_month(date)?;
                continue;
            }
            if !self.day_matches(date) {
                date = date.checked_add_days(chrono::Days::new(1))?;
                continue;
            }

            let same_day = date == start.date_naive();
            let from_hour = if same_day { start.hour() } else { 0 };
            for hour in mask_values(self.hours, 0) {
                if hour < from_hour {
                    continue;
                }
                let from_minute = if same_day && hour == start.hour() {
                    start.minute()
                } else {
                    0
                };
                for minute in mask_values(self.minutes, 0) {
                    if minute < from_minute {
                        continue;
                    }
                    let naive = date.and_hms_opt(hour, minute, 0)?;
                    // Skip wall-clock times that do not exist (DST gap) and
                    // take the earlier option of an ambiguous time.
                    match Local.from_local_datetime(&naive) {
                        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => return Some(dt),
                        LocalResult::None => continue,
                    }
                }
            }
            date = date.checked_add_days(chrono::Days::new(1))?;
        }
        None
    }

    /// Human-readable summary of the schedule, e.g. "Daily at 08:30".
    pub fn describe(&self) -> String {
        if self.reboot {
            return "At every reboot".to_string();
        }
        let minutes = mask_values(self.minutes, 0);
        let hours = mask_values(self.hours, 0);
        let doms = mask_values(self.doms, 1);
        let months = mask_values(self.months, 1);
        let dows = mask_values(self.dows, 0);

        let every_day = self.dom_star && self.dow_star && self.months == mask_all(1, 12);
        let single_time = match (hours.as_slice(), minutes.as_slice()) {
            ([h], [m]) => Some((h, m)),
            _ => None,
        };

        if every_day && self.minutes == mask_all(0, 59) && self.hours == mask_all(0, 23) {
            return "Every minute".to_string();
        }

        // Every N minutes: the minute set is exactly 0, N, 2N, ... across all hours.
        if every_day
            && self.hours == mask_all(0, 23)
            && let Some(n) = uniform_step(&minutes, 0, 59)
        {
            return format!("Every {n} minutes");
        }

        // Fixed minute, every hour (or every N hours).
        if every_day && minutes.len() == 1 {
            if self.hours == mask_all(0, 23) {
                return format!("Every hour at :{:02}", minutes[0]);
            }
            if let Some(n) = uniform_step(&hours, 0, 23) {
                return format!("Every {n} hours at :{:02}", minutes[0]);
            }
        }

        // Daily at HH:MM.
        if every_day && let Some((&h, &m)) = single_time {
            return format!("Daily at {h:02}:{m:02}");
        }

        // Weekly: restricted weekdays, every week of every month.
        if self.dom_star
            && self.months == mask_all(1, 12)
            && let Some((&h, &m)) = single_time
        {
            let days = describe_dows(&dows);
            return format!("{days} at {h:02}:{m:02}");
        }

        // Monthly on a fixed day.
        if self.dow_star
            && self.months == mask_all(1, 12)
            && doms.len() == 1
            && let Some((&h, &m)) = single_time
        {
            return format!("Monthly on day {} at {h:02}:{m:02}", doms[0]);
        }

        // Yearly on a fixed date.
        if months.len() == 1
            && doms.len() == 1
            && self.dows == mask_all(0, 6)
            && let Some((&h, &m)) = single_time
        {
            let month = MONTH_NAMES[(months[0] - 1) as usize];
            return format!("Yearly on {month} {} at {h:02}:{m:02}", doms[0]);
        }

        format!("Custom schedule ({})", self.expr)
    }
}

/// Parse one cron field into a bitmask over `min..=max` (bit `v - min`).
///
/// `names` maps case-insensitive 3-letter names to values starting at
/// `first_value` (months: Jan=1, weekdays: Sun=0).
fn parse_field(
    field: &str,
    min: u32,
    max: u32,
    names: Option<(&[&str], u32)>,
    expr: &str,
    label: &str,
) -> Result<u64> {
    let invalid = |part: &str, reason: String| {
        AppError::InvalidSchedule(
            expr.to_string(),
            format!("{label} field: '{part}': {reason}"),
        )
    };

    let mut mask: u64 = 0;
    for part in field.split(',') {
        let (range_part, step) = match part.split_once('/') {
            Some((range, step_str)) => {
                let step: u32 = step_str
                    .parse()
                    .map_err(|_| invalid(part, format!("invalid step '{step_str}'")))?;
                if step == 0 {
                    return Err(invalid(part, "step must be >= 1".to_string()));
                }
                (range, step)
            }
            None => (part, 1),
        };

        let (start, end) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            let start = parse_value(a, names)
                .map_err(|e| invalid(part, format!("bad range start: {e}")))?;
            let end =
                parse_value(b, names).map_err(|e| invalid(part, format!("bad range end: {e}")))?;
            (start, end)
        } else {
            // A bare value with a step (`N/2`) means N..max/2, matching vixie cron.
            let v = parse_value(range_part, names).map_err(|e| invalid(part, e))?;
            if part.contains('/') { (v, max) } else { (v, v) }
        };

        if start > end {
            return Err(invalid(part, "range start exceeds end".to_string()));
        }
        if start < min || end > max {
            return Err(invalid(part, format!("out of range {min}-{max}")));
        }

        let mut v = start;
        while v <= end {
            mask |= 1 << (v - min);
            v += step;
        }
    }
    Ok(mask)
}

fn parse_value(token: &str, names: Option<(&[&str], u32)>) -> std::result::Result<u32, String> {
    if let Some((names, first_value)) = names
        && let Some(i) = names.iter().position(|&n| n.eq_ignore_ascii_case(token))
    {
        return Ok(first_value + i as u32);
    }
    token
        .parse::<u32>()
        .map_err(|_| format!("unexpected value '{token}'"))
}

/// Map day-of-week bit 7 (Sunday) onto bit 0.
fn normalize_dow(raw: u64) -> u64 {
    let mut mask = raw & 0x7f;
    if raw & (1 << 7) != 0 {
        mask |= 1;
    }
    mask
}

/// All bits set over `min..=max`.
fn mask_all(min: u32, max: u32) -> u64 {
    let n = max - min + 1;
    if n >= 64 { u64::MAX } else { (1 << n) - 1 }
}

/// Sorted values whose bits are set in `mask` (value = index + `min`).
fn mask_values(mask: u64, min: u32) -> Vec<u32> {
    (0..64)
        .filter(|&i| mask & (1 << i) != 0)
        .map(|i| i + min)
        .collect()
}

/// If `values` are exactly `start, start+n, start+2n, ...` up to `max`,
/// return `n`.
fn uniform_step(values: &[u32], start: u32, max: u32) -> Option<u32> {
    let (&first, rest) = values.split_first()?;
    if first != start {
        return None;
    }
    let n = rest.first().copied()? - first;
    if n == 0 {
        return None;
    }
    let complete = values
        .iter()
        .enumerate()
        .all(|(i, &v)| v == start + n * (i as u32))
        && values.last() == Some(&(start + n * ((max - start) / n)));
    if complete { Some(n) } else { None }
}

fn describe_dows(dows: &[u32]) -> String {
    if dows.len() >= 3 && dows.windows(2).all(|w| w[1] == w[0] + 1) {
        return format!(
            "Every {} to {}",
            DOW_FULL_NAMES[dows[0] as usize],
            DOW_FULL_NAMES[*dows.last().unwrap() as usize]
        );
    }
    let names: Vec<String> = dows
        .iter()
        .map(|&d| format!("{}s", DOW_FULL_NAMES[d as usize]))
        .collect();
    match names.as_slice() {
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        [] => String::new(),
    }
}

fn first_of_next_month(date: NaiveDate) -> Option<NaiveDate> {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1)
}

/// Validate a standard 5-field cron expression.
pub fn validate_schedule(expr: &str) -> Result<()> {
    CronSchedule::parse(expr).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn parse_valid_schedules() {
        for expr in [
            "* * * * *",
            "0 * * * *",
            "*/5 9-17 * * 1-5",
            "0 0 1 JAN *",
            "*/10 9-17 * * MON-FRI",
            "0,30 8-20 * * *",
            "5/15 * * * *", // vixie: 5..59 step 15
            "0 0 * * 7",    // 7 = Sunday
            "59 23 31 12 SAT",
            "0 12 * Feb,Dec *",
        ] {
            assert!(CronSchedule::parse(expr).is_ok(), "{expr} should be valid");
        }
    }

    #[test]
    fn parse_invalid_schedules() {
        for expr in [
            "* * * *",
            "a * * * *",
            "* * * * * * *",
            "60 * * * *",
            "0 0 0 * *",
            "0 24 * * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "0 0 32 * *",
            "0 0 1 FOO *",
            "1-2-3 * * * *",
            "*/ * * * *",
        ] {
            assert!(
                CronSchedule::parse(expr).is_err(),
                "{expr} should be invalid"
            );
        }
    }

    #[test]
    fn dow_7_is_sunday() {
        let sched = CronSchedule::parse("0 0 * * 7").unwrap();
        assert_eq!(sched.dows, 0b0000_0001); // bit 0 = Sunday
        let sched = CronSchedule::parse("0 0 * * 0,7").unwrap();
        assert_eq!(sched.dows, 0b0000_0001);
    }

    #[test]
    fn next_run_every_15_minutes() {
        let sched = CronSchedule::parse("*/15 * * * *").unwrap();
        let next = sched.next_after(&local(2026, 1, 15, 10, 7)).unwrap();
        assert_eq!(next, local(2026, 1, 15, 10, 15));
    }

    #[test]
    fn next_run_includes_boundary_minute() {
        // 10:14:30 truncated+1 -> candidate 10:15 must itself match.
        let sched = CronSchedule::parse("*/15 * * * *").unwrap();
        let after = local(2026, 1, 15, 10, 14) + Duration::seconds(30);
        let next = sched.next_after(&after).unwrap();
        assert_eq!(next, local(2026, 1, 15, 10, 15));
    }

    #[test]
    fn next_run_weekday_hours() {
        // 2026-01-16 is a Friday; 09:00-17:00 weekdays only.
        let sched = CronSchedule::parse("0 9-17 * * 1-5").unwrap();
        let next = sched.next_after(&local(2026, 1, 16, 18, 0)).unwrap();
        assert_eq!(next, local(2026, 1, 19, 9, 0)); // Monday
    }

    #[test]
    fn next_run_dom_dow_is_or_not_and() {
        // dom=13 and dow=Friday are both restricted: cron matches either.
        // 2026-01-02 is a Friday.
        let sched = CronSchedule::parse("0 12 13 * 5").unwrap();
        let next = sched.next_after(&local(2026, 1, 1, 0, 0)).unwrap();
        assert_eq!(next, local(2026, 1, 2, 12, 0));
    }

    #[test]
    fn next_run_leap_day() {
        let sched = CronSchedule::parse("30 2 29 2 *").unwrap();
        let next = sched.next_after(&local(2026, 3, 1, 0, 0)).unwrap();
        assert_eq!(next, local(2028, 2, 29, 2, 30));
    }

    #[test]
    fn next_run_impossible_date_is_none() {
        let sched = CronSchedule::parse("0 0 31 2 *").unwrap();
        assert!(sched.next_after(&local(2026, 1, 1, 0, 0)).is_none());
    }

    #[test]
    fn describe_common_patterns() {
        let cases = [
            ("* * * * *", "Every minute"),
            ("*/5 * * * *", "Every 5 minutes"),
            ("0 * * * *", "Every hour at :00"),
            ("30 */2 * * *", "Every 2 hours at :30"),
            ("30 8 * * *", "Daily at 08:30"),
            ("0 9 * * 1", "Mondays at 09:00"),
            ("0 9 * * 1-5", "Every Monday to Friday at 09:00"),
            ("30 8 * * 0,6", "Sundays and Saturdays at 08:30"),
            ("0 0 1 * *", "Monthly on day 1 at 00:00"),
            ("0 0 1 1 *", "Yearly on Jan 1 at 00:00"),
        ];
        for (expr, expected) in cases {
            let sched = CronSchedule::parse(expr).unwrap();
            assert_eq!(sched.describe(), expected, "describe({expr})");
        }
    }

    #[test]
    fn describe_falls_back_for_custom() {
        let sched = CronSchedule::parse("1,2 3,4 * * 1,3").unwrap();
        assert!(sched.describe().starts_with("Custom schedule"));
    }

    #[test]
    fn parse_nicknames() {
        for expr in [
            "@yearly",
            "@annually",
            "@monthly",
            "@weekly",
            "@daily",
            "@midnight",
            "@hourly",
            "@reboot",
            "@DAILY", // case-insensitive
        ] {
            assert!(CronSchedule::parse(expr).is_ok(), "{expr} should be valid");
        }
        for expr in ["@fortnightly", "@daily * * * *", "@reboot extra"] {
            assert!(
                CronSchedule::parse(expr).is_err(),
                "{expr} should be invalid"
            );
        }
    }

    #[test]
    fn nickname_describe_and_next_run() {
        let daily = CronSchedule::parse("@daily").unwrap();
        assert_eq!(daily.describe(), "Daily at 00:00");
        assert_eq!(
            daily.next_after(&local(2026, 1, 15, 10, 7)).unwrap(),
            local(2026, 1, 16, 0, 0)
        );

        let weekly = CronSchedule::parse("@weekly").unwrap();
        assert_eq!(weekly.describe(), "Sundays at 00:00");

        let reboot = CronSchedule::parse("@reboot").unwrap();
        assert!(reboot.is_reboot());
        assert_eq!(reboot.describe(), "At every reboot");
        assert!(reboot.next_after(&local(2026, 1, 1, 0, 0)).is_none());
        assert!(!daily.is_reboot());
    }

    #[test]
    fn uniform_step_detection() {
        assert_eq!(uniform_step(&[0, 15, 30, 45], 0, 59), Some(15));
        assert_eq!(uniform_step(&[0, 10, 20, 30, 40, 50], 0, 59), Some(10));
        assert_eq!(uniform_step(&[5, 10, 15], 0, 59), None);
        assert_eq!(uniform_step(&[0, 1, 2], 0, 59), None);
        assert_eq!(uniform_step(&[0], 0, 59), None);
    }
}
