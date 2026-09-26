# Cron Job Manager

![CI](https://github.com/yikuide-lab/cronjob-manager/actions/workflows/ci.yml/badge.svg)

A desktop GUI application written in Rust for managing cron jobs on Linux systems. It supports viewing and editing both the current user's crontab and system-level cron entries (`/etc/crontab` and `/etc/cron.d/`).

## Features

- **User crontab**: read/write via the system `crontab` command.
- **System crontab**: read/write `/etc/crontab` (atomically — temp file + rename, preserving permissions).
- **cron.d directory**: read/write individual files under `/etc/cron.d/`; files that cron itself ignores (names with dots, backups, dotfiles) are skipped.
- **Permission-aware**: system-level entries are read-only unless the app is run as root.
- **Non-blocking I/O**: sources are read on background threads, so a slow or hung `crontab` invocation never freezes the UI.
- **External-change detection**: every 20s the app re-reads all sources in the background and reloads them (with a notice) if they changed outside the app — polling covers the user crontab spool, which inotify cannot watch.
- **Add / Edit / Delete / Toggle** cron jobs through a simple egui interface.
- **Cron expression validation** with inline feedback in the add/edit dialog. Supports the 5-field syntax (names, steps, ranges, `0`/`7` Sunday, dom/dow OR rule) and the `@`-nicknames (`@reboot`, `@daily`, `@hourly`, ...).
- **Schedule description**: plain-language summary (`0 9 * * 1-5` → "Every Monday to Friday at 09:00", `@reboot` → "At every reboot").
- **Next-run preview**: shows when each job will fire next (handles the dom/dow OR rule, month/day names, DST gaps, and leap-day schedules); `@reboot` jobs are marked "after reboot".
- **Lossless round-trip**: unknown lines (comments, environment variables, blanks) are preserved, and command text keeps its original internal spacing when a file is rewritten.

## Requirements

- Rust 1.85+ (edition 2024)
- Linux with `crontab` and `cron` installed
- A display server or `Xvfb` for headless environments

## Build

```bash
cargo build --release
```

## Run

```bash
cargo run
```

To manage system cron entries, run with root privileges:

```bash
sudo cargo run
```

## Test

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
```

## Project Structure

```
src/
├── main.rs          # Application entry point
├── app.rs           # egui application state and UI
├── lib.rs           # Module exports
├── error.rs         # Error types
├── privilege.rs     # Root privilege detection
├── cron/            # Cron domain model
│   ├── job.rs       #   CronJob / CrontabFile / CronSource
│   ├── parser.rs    #   crontab text -> CrontabFile
│   ├── schedule.rs  #   expression parse/validate, describe, next-run
│   └── serializer.rs#   CrontabFile -> crontab text
├── crontab/         # User crontab operations (via `crontab` command)
└── system_cron/     # /etc/crontab and /etc/cron.d operations (atomic writes)
```

## Safety Notes

- System-level modifications require root. The UI disables edit controls and shows a warning when running without root.
- Auto-reload only swaps sources when nothing is in flight and no dialog is open, so it can never clobber an edit in progress; the previously selected source stays selected across reloads.
- All system-file writes are atomic (write to a dot-prefixed temp file in the same directory, fsync, rename), so a crash mid-save can never truncate `/etc/crontab` or a `/etc/cron.d/` file. Existing permissions are preserved.
- The parser preserves unknown lines (comments, environment variables, blank lines) so writing back a crontab file does not destroy unrelated content. Command text is kept verbatim (including internal spacing) so rewriting a file never mangles other jobs.
- Disabled jobs are represented by commenting the line out (`# ...`); toggling back removes the prefix. Month/weekday names in disabled lines (`# 0 12 * * MON cmd`) and nickname schedules (`# @reboot cmd`) are recognized and survive reloads.
