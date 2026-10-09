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

pub fn init(dir: &Path) {
    let path = dir.join("dsz.log");
    // Keep one previous log around.
    if path.exists() {
        let _ = std::fs::rename(&path, dir.join("dsz.previous.log"));
    }
    let file = std::fs::File::create(&path).ok();
    let logger = LOGGER.get_or_init(|| Logger {
        file: Mutex::new(file),
        ring: Mutex::new(VecDeque::with_capacity(RING)),
    });
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Debug);
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
