//! Log files, one per local day, in the app log folder (macOS
//! `~/Library/Logs/app.biwrite.desktop/`, Windows
//! `%LOCALAPPDATA%\app.biwrite.desktop\logs\`), named
//! `biwrite-YYYY-MM-DD.log`.
//!
//! - At most [`MAX_DAY_BYTES`] per day. Past that, one line says so and the
//!   rest of the day is dropped.
//! - Files older than [`KEEP_DAYS`] days are deleted at startup and whenever
//!   the day changes. Only files named like ours are ever touched.
//! - BiWrite's own messages are kept from `info` up, other crates' (tauri,
//!   keyring, ...) from `warn` up. Lines also go to stderr.
//! - Messages are what the UI shows (errors, notices, file names). Document
//!   text appears only as far as an error quotes it (a placeholder error
//!   names the formula it lost). Every line passes through the API-key
//!   redaction again.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use log::{Level, LevelFilter, Log, Metadata, Record};
use time::{Date, Duration, Month, OffsetDateTime, UtcOffset};

/// Days of logs kept, today included.
pub const KEEP_DAYS: i64 = 7;
/// Size limit of one day's file.
pub const MAX_DAY_BYTES: u64 = 5 * 1024 * 1024;

const PREFIX: &str = "biwrite-";
const SUFFIX: &str = ".log";

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Remember the local UTC offset at startup, in case reading it fails later.
fn capture_local_offset() {
    LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC));
}

/// Local time, read per line so a daylight-saving change applies at once.
fn now_local() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| {
        let offset = LOCAL_OFFSET.get().copied().unwrap_or(UtcOffset::UTC);
        OffsetDateTime::now_utc().to_offset(offset)
    })
}

/// `2026-10-09 18:22:01.123 +08:00`
fn timestamp(t: OffsetDateTime) -> String {
    let offset = t.offset();
    format!(
        "{} {:02}:{:02}:{:02}.{:03} {}{:02}:{:02}",
        date_str(t.date()),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond(),
        if offset.is_negative() { '-' } else { '+' },
        offset.whole_hours().unsigned_abs(),
        offset.minutes_past_hour().unsigned_abs(),
    )
}

fn date_str(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

fn file_name(date: Date) -> String {
    format!("{PREFIX}{}{SUFFIX}", date_str(date))
}

/// The date of one of our log files, from its name.
fn file_date(name: &str) -> Option<Date> {
    let stem = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let mut parts = stem.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let month = Month::try_from(m.parse::<u8>().ok()?).ok()?;
    Date::from_calendar_date(y.parse().ok()?, month, d.parse().ok()?).ok()
}

/// Delete our log files from before the last [`KEEP_DAYS`] days. Returns how
/// many were deleted.
pub fn delete_old(dir: &Path, today: Date) -> usize {
    let oldest_kept = today - Duration::days(KEEP_DAYS - 1);
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .and_then(file_date)
                .is_some_and(|d| d < oldest_kept)
        })
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

/// The current day's file.
struct Day {
    date: Option<Date>,
    file: Option<File>,
    written: u64,
    capped: bool,
}

/// Appends lines to the file of the line's day, within the daily limit.
pub struct DailyLog {
    dir: PathBuf,
    day: Mutex<Day>,
}

impl DailyLog {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            day: Mutex::new(Day {
                date: None,
                file: None,
                written: 0,
                capped: false,
            }),
        }
    }

    pub fn write_line(&self, now: OffsetDateTime, line: &str) {
        let mut day = self.day.lock().unwrap_or_else(PoisonError::into_inner);
        if day.date != Some(now.date()) {
            self.open(&mut day, now.date());
        }
        let Day {
            file,
            written,
            capped,
            ..
        } = &mut *day;
        let Some(file) = file.as_mut() else {
            return;
        };
        // Lines are dropped only once the file has reached the limit (the
        // last one may overshoot), so a restart on a full day sees it full.
        if *written >= MAX_DAY_BYTES {
            if !*capped {
                *capped = true;
                let _ = writeln!(
                    file,
                    "{} WARN  biwrite: the log reached {} MB for today; further messages are dropped until tomorrow",
                    timestamp(now),
                    MAX_DAY_BYTES / (1024 * 1024)
                );
            }
            return;
        }
        if writeln!(file, "{line}").is_ok() {
            *written += line.len() as u64 + 1;
        }
    }

    /// Switch to `date`'s file (appending) and delete expired ones.
    fn open(&self, day: &mut Day, date: Date) {
        delete_old(&self.dir, date);
        let file = fs::create_dir_all(&self.dir).and_then(|()| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(file_name(date)))
        });
        let written = file
            .as_ref()
            .ok()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        *day = Day {
            date: Some(date),
            file: file.ok(),
            written,
            capped: written >= MAX_DAY_BYTES,
        };
    }
}

struct Logger {
    /// Set once the log folder is known (Tauri's setup); until then lines
    /// only go to stderr.
    file: OnceLock<DailyLog>,
}

static LOGGER: Logger = Logger {
    file: OnceLock::new(),
};

impl Log for Logger {
    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        let max = if meta.target().starts_with("biwrite") {
            Level::Info
        } else {
            Level::Warn
        };
        meta.level() <= max
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = now_local();
        let line = biwrite_providers::redact(
            &format!(
                "{} {:<5} {}: {}",
                timestamp(now),
                record.level(),
                record.target(),
                record.args()
            ),
            "",
        );
        // Never panics, even without a console (Windows release builds).
        let _ = writeln!(std::io::stderr(), "{line}");
        if let Some(file) = self.file.get() {
            file.write_line(now, &line);
        }
    }

    fn flush(&self) {}
}

/// Install the logger (stderr until [`attach_dir`]) and a panic hook that
/// logs panics with a backtrace. Call first thing at startup, so failures
/// before the app is set up are still reported.
pub fn init() {
    capture_local_offset();
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!(target: "biwrite::panic", "{info}\n{backtrace}");
        default_hook(info);
    }));
}

/// Start writing the daily files in `dir`.
pub fn attach_dir(dir: PathBuf) {
    let _ = LOGGER.file.set(DailyLog::new(dir));
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("biwrite-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn one_file_per_day() {
        let dir = temp_dir("days");
        let log = DailyLog::new(dir.clone());
        log.write_line(datetime!(2026-10-09 23:59:59 +8), "late");
        log.write_line(datetime!(2026-10-10 00:00:01 +8), "early");
        log.write_line(datetime!(2026-10-10 09:00:00 +8), "later");
        assert_eq!(
            fs::read_to_string(dir.join("biwrite-2026-10-09.log")).unwrap(),
            "late\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("biwrite-2026-10-10.log")).unwrap(),
            "early\nlater\n"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_day_stops_at_the_size_limit() {
        let dir = temp_dir("cap");
        let log = DailyLog::new(dir.clone());
        // 1,000-byte lines don't divide the limit evenly.
        let line = "x".repeat(999);
        let day = datetime!(2026-10-09 12:00:00 +8);
        for _ in 0..(MAX_DAY_BYTES / 1000 + 50) {
            log.write_line(day, &line);
        }
        let text = fs::read_to_string(dir.join("biwrite-2026-10-09.log")).unwrap();
        // At most one line over, plus the notice.
        assert!(text.len() as u64 >= MAX_DAY_BYTES);
        assert!(
            text.len() as u64 <= MAX_DAY_BYTES + 1024 + 200,
            "{}",
            text.len()
        );
        assert_eq!(text.matches("further messages are dropped").count(), 1);
        // The next day starts fresh.
        log.write_line(datetime!(2026-10-10 00:00:00 +8), "new day");
        assert_eq!(
            fs::read_to_string(dir.join("biwrite-2026-10-10.log")).unwrap(),
            "new day\n"
        );
        // A restart on a full day doesn't write more either.
        let again = DailyLog::new(dir.clone());
        again.write_line(day, "more");
        let after = fs::read_to_string(dir.join("biwrite-2026-10-09.log")).unwrap();
        assert_eq!(after.len(), text.len());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_our_old_files_are_deleted() {
        let dir = temp_dir("cleanup");
        for name in [
            "biwrite-2026-10-02.log",
            "biwrite-2026-10-03.log",
            "biwrite-2026-10-09.log",
            "biwrite-2026-9-01.log",
            "biwrite-latest.log",
            "notes-2020-01-01.log",
            "biwrite-2020-01-01.log.bak",
        ] {
            fs::write(dir.join(name), "x").unwrap();
        }
        let log = DailyLog::new(dir.clone());
        log.write_line(datetime!(2026-10-09 08:00:00 +8), "today");
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        // Kept: the last 7 days (10-03 … 10-09) and anything not ours.
        assert_eq!(
            left,
            [
                "biwrite-2020-01-01.log.bak",
                "biwrite-2026-10-03.log",
                "biwrite-2026-10-09.log",
                "biwrite-2026-9-01.log",
                "biwrite-latest.log",
                "notes-2020-01-01.log",
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lines_are_redacted_and_filtered() {
        let dir = temp_dir("logger");
        let logger = Logger {
            file: OnceLock::new(),
        };
        let _ = logger.file.set(DailyLog::new(dir.clone()));
        let record = |level: Level, target: &'static str, message: &'static str| {
            logger.log(
                &Record::builder()
                    .level(level)
                    .target(target)
                    .args(format_args!("{message}"))
                    .build(),
            );
        };
        record(
            Level::Warn,
            "biwrite_lib::sink",
            "failed: key sk-abcdefghijklmnopqrstuvwxyz",
        );
        record(Level::Info, "biwrite_lib::commands", "saved paper.tex");
        record(Level::Info, "tauri::manager", "chatty third-party info");
        record(Level::Warn, "tauri::manager", "third-party warning");
        let file = dir.join(file_name(now_local().date()));
        let text = fs::read_to_string(file).unwrap();
        assert!(
            text.contains("WARN  biwrite_lib::sink: failed: key [redacted]"),
            "{text}"
        );
        assert!(!text.contains("abcdefghijklmnop"));
        assert!(text.contains("INFO  biwrite_lib::commands: saved paper.tex"));
        assert!(!text.contains("chatty"));
        assert!(text.contains("third-party warning"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn timestamps_and_names() {
        assert_eq!(
            timestamp(datetime!(2026-10-09 08:05:03.042 +8)),
            "2026-10-09 08:05:03.042 +08:00"
        );
        assert_eq!(
            timestamp(datetime!(2026-01-02 03:04:05 -5:30)),
            "2026-01-02 03:04:05.000 -05:30"
        );
        assert_eq!(
            file_date("biwrite-2026-02-28.log"),
            Some(datetime!(2026-02-28 0:00 UTC).date())
        );
        assert_eq!(file_date("biwrite-2026-02-30.log"), None);
    }
}
