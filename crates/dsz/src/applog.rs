//! Logger: a file in the data directory plus an in-memory ring the UI shows.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use parking_lot::Mutex;

pub struct LogLine {
    pub level: log::Level,
    pub time: String,
    pub text: String,
}

struct Logger {
    file: Mutex<Option<std::fs::File>>,
    ring: Mutex<VecDeque<LogLine>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

const RING: usize = 600;
/// Logs kept from earlier runs: `dsz.previous.log` (the last run), then
/// `dsz.previous.2.log` up to this number, oldest last.
const KEEP: usize = 5;
/// Earlier runs' logs past the last one are dropped, oldest first, once
/// they would take more than this together (a log flooded by a fault can
/// be large). The last run's log is always kept.
const KEEP_BYTES: u64 = 50 * 1024 * 1024;

fn clock() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    // Local offset is not worth a dependency; the file name carries the date.
    let day = secs % 86400.0;
    let h = (day / 3600.0) as u32;
    let m = ((day % 3600.0) / 60.0) as u32;
    let s = day % 60.0;
    format!("{h:02}:{m:02}:{s:06.3} UTC")
}

impl log::Log for Logger {
    fn enabled(&self, md: &log::Metadata) -> bool {
        md.level() <= log::Level::Info || md.target().starts_with("dsz")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Keep noisy dependency logs out.
        if record.level() > log::Level::Warn && !record.target().starts_with("dsz") {
            return;
        }
        let line = LogLine {
            level: record.level(),
            time: clock(),
            text: format!("{}", record.args()),
        };
        if let Some(f) = self.file.lock().as_mut() {
            let _ = writeln!(f, "{} {:<5} {}", line.time, line.level, line.text);
        }
        let mut r = self.ring.lock();
        if r.len() >= RING {
            r.pop_front();
        }
        r.push_back(line);
    }

    fn flush(&self) {
        if let Some(f) = self.file.lock().as_mut() {
            let _ = f.flush();
        }
    }
}

/// File name of the log from `n` runs ago (1 is the last run).
fn previous_name(n: usize) -> String {
    if n <= 1 {
        "dsz.previous.log".to_string()
    } else {
        format!("dsz.previous.{n}.log")
    }
}

/// Make room for a new `dsz.log`: it becomes `dsz.previous.log`, and the
/// earlier ones move one place down, up to `keep` of them and, past the
/// first, `max_bytes` together. Best effort: a file that can't be moved or
/// removed (locked, gone) costs at most that file, never the start-up.
fn rotate(dir: &Path, keep: usize, max_bytes: u64) {
    let path = |n: usize| dir.join(previous_name(n));
    let current = dir.join("dsz.log");
    if keep == 0 || !current.exists() {
        return;
    }
    let _ = std::fs::remove_file(path(keep));
    for n in (1..keep).rev() {
        let from = path(n);
        if from.exists() {
            let _ = std::fs::rename(&from, path(n + 1));
        }
    }
    if std::fs::rename(&current, path(1)).is_err() {
        // Still keep what it says before `init` truncates it.
        let _ = std::fs::copy(&current, path(1));
    }
    let mut total = 0u64;
    for n in 1..=keep {
        let Ok(md) = std::fs::metadata(path(n)) else {
            continue;
        };
        total = total.saturating_add(md.len());
        if n > 1 && total > max_bytes {
            for m in n..=keep {
                let _ = std::fs::remove_file(path(m));
            }
            break;
        }
    }
}

pub fn init(dir: &Path) {
    let path = dir.join("dsz.log");
    rotate(dir, KEEP, KEEP_BYTES);
    let file = std::fs::File::create(&path).ok();
    let logger = LOGGER.get_or_init(|| Logger {
        file: Mutex::new(file),
        ring: Mutex::new(VecDeque::with_capacity(RING)),
    });
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Debug);
}

/// Stop writing the log file (its folder is about to move).
pub fn close() {
    if let Some(l) = LOGGER.get() {
        *l.file.lock() = None;
    }
}

/// Copy of the recent lines for the UI, newest last.
pub fn recent(max: usize, min_level: log::Level) -> Vec<(log::Level, String, String)> {
    let Some(l) = LOGGER.get() else {
        return Vec::new();
    };
    let r = l.ring.lock();
    r.iter()
        .filter(|x| x.level <= min_level)
        .rev()
        .take(max)
        .map(|x| (x.level, x.time.clone(), x.text.clone()))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{previous_name, rotate};
    use std::path::{Path, PathBuf};

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsz-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(dir: &Path, name: &str) -> Option<String> {
        std::fs::read_to_string(dir.join(name)).ok()
    }

    #[test]
    fn previous_names() {
        assert_eq!(previous_name(1), "dsz.previous.log");
        assert_eq!(previous_name(2), "dsz.previous.2.log");
        assert_eq!(previous_name(5), "dsz.previous.5.log");
    }

    #[test]
    fn rotation_keeps_the_last_runs_newest_first() {
        let dir = temp("rotate");
        for run in 1..=7 {
            rotate(&dir, 5, u64::MAX);
            std::fs::write(dir.join("dsz.log"), format!("run {run}")).unwrap();
        }
        rotate(&dir, 5, u64::MAX);
        assert_eq!(read(&dir, "dsz.log"), None);
        assert_eq!(read(&dir, "dsz.previous.log").as_deref(), Some("run 7"));
        assert_eq!(read(&dir, "dsz.previous.2.log").as_deref(), Some("run 6"));
        assert_eq!(read(&dir, "dsz.previous.5.log").as_deref(), Some("run 3"));
        assert_eq!(read(&dir, "dsz.previous.6.log"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_without_a_log_leaves_the_old_ones() {
        let dir = temp("rotate-none");
        std::fs::write(dir.join("dsz.previous.log"), "a").unwrap();
        rotate(&dir, 5, u64::MAX);
        assert_eq!(read(&dir, "dsz.previous.log").as_deref(), Some("a"));
        assert_eq!(read(&dir, "dsz.previous.2.log"), None);
        // A gap (one removed by hand) is carried along, not an error.
        std::fs::write(dir.join("dsz.previous.3.log"), "c").unwrap();
        std::fs::write(dir.join("dsz.log"), "new").unwrap();
        rotate(&dir, 5, u64::MAX);
        assert_eq!(read(&dir, "dsz.previous.log").as_deref(), Some("new"));
        assert_eq!(read(&dir, "dsz.previous.2.log").as_deref(), Some("a"));
        assert_eq!(read(&dir, "dsz.previous.3.log"), None);
        assert_eq!(read(&dir, "dsz.previous.4.log").as_deref(), Some("c"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_caps_the_total_size() {
        let dir = temp("rotate-cap");
        for (name, len) in [("dsz.previous.log", 40), ("dsz.previous.2.log", 30), ("dsz.previous.3.log", 30)] {
            std::fs::write(dir.join(name), "x".repeat(len)).unwrap();
        }
        // The last run's log is kept whatever its size.
        std::fs::write(dir.join("dsz.log"), "y".repeat(120)).unwrap();
        rotate(&dir, 5, 100);
        assert_eq!(read(&dir, "dsz.previous.log").map(|s| s.len()), Some(120));
        for n in 2..=5 {
            assert_eq!(read(&dir, &previous_name(n)), None, "{n}");
        }
        // Small ones: as many as fit, oldest dropped.
        std::fs::write(dir.join("dsz.previous.log"), "x".repeat(40)).unwrap();
        std::fs::write(dir.join("dsz.previous.2.log"), "x".repeat(30)).unwrap();
        std::fs::write(dir.join("dsz.log"), "y".repeat(20)).unwrap();
        rotate(&dir, 5, 100);
        assert_eq!(read(&dir, "dsz.previous.log").map(|s| s.len()), Some(20));
        assert_eq!(read(&dir, "dsz.previous.2.log").map(|s| s.len()), Some(40));
        assert_eq!(read(&dir, "dsz.previous.3.log").map(|s| s.len()), Some(30));
        std::fs::write(dir.join("dsz.log"), "y".repeat(20)).unwrap();
        rotate(&dir, 5, 100);
        assert_eq!(read(&dir, "dsz.previous.3.log").map(|s| s.len()), Some(40));
        assert_eq!(read(&dir, "dsz.previous.4.log"), None, "20 + 20 + 40 + 30 is over 100");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_the_log_file() {
        let dir = std::env::temp_dir().join(format!("dsz-log-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("dsz.log"), "old\n").unwrap();
        super::init(&dir);
        log::info!("hello from the test");
        let text = std::fs::read_to_string(dir.join("dsz.log")).unwrap();
        let prev =
            std::fs::read_to_string(dir.join("dsz.previous.log")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(prev, "old\n");
        assert!(text.contains("hello from the test"), "log was: {text:?}");
    }
}
