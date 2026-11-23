//! Keep reading a DualSense while Windows holds back its input stream
//! (issue 63). With Xbox Game Bar or the gamepad touch keyboard open,
//! newer Windows 11 builds stop completing the HID input reads of other
//! apps, so the link showed No signal until the overlay closed. Asking for
//! the current input report (`IOCTL_HID_GET_INPUT_REPORT`) still returns
//! fresh, CRC-valid state; DS4Windows found the same and reads that way
//! during the gap (Pav-Osmolski/DS4Windows issue 107, PR 111).
//!
//! Always on for DualSense and Edge. The reader asks only once the stream
//! has been silent for [`STALL`]. A failed ask stops asking until the
//! stream delivers again, so a controller that is out of range or asleep
//! gets one request per silence, not a stream, and "No signal" still
//! appears after the usual quiet timeout. DualShock 4 is unchanged: the
//! long-standing 100 ms read, and no requests.
//!
//! When an ask fails, [`Stall::fail`] yields one always-on log line for
//! that silence: the Win32 error, a timeout, a short or wrong report, a
//! CRC failure, or that the request was not attempted, plus the connection
//! and report id. Later failures in the same silence are silent.

use std::io;
use std::time::{Duration, Instant};

use ds_proto::{Connection, Model};

/// Stream read timeout when this model is not asked for (DualShock 4,
/// or after an ask got no answer): the reader's long-standing value.
pub const READ_MS: u32 = 100;
/// Stream read timeout while watching for a silence, so one is noticed
/// soon after [`STALL`].
const WATCH_MS: u32 = 16;
/// Stream read timeout between asks while the stream is held back. It also
/// spaces the asks out, so they never become a busy loop.
const ASK_GAP_MS: u32 = 4;
/// How long the controller may send nothing before the reader asks. The
/// controller streams every few milliseconds even when idle, so 40 ms is
/// many missed reports, not ordinary jitter; "No signal" is 1.5 s later.
pub const STALL: Duration = Duration::from_millis(40);
/// Longest wait for one current-report answer.
pub const ASK_TIMEOUT_MS: u32 = 32;
/// When the stream comes back, its queue still holds reports from before
/// the state already taken from asks; they are dropped for at most this
/// long (a controller whose clock jumped is then read as it comes).
const STALE_FOR: Duration = Duration::from_millis(250);

/// The input report to ask for: the one the stream carries. DualShock 4
/// reports aren't covered (no one has seen them held back with DSZ yet).
pub fn report_id(model: Model, conn: Connection) -> Option<u8> {
    if model.is_dualshock4() {
        return None;
    }
    Some(match conn {
        Connection::Bluetooth => 0x31,
        Connection::Usb => 0x01,
    })
}

/// Why one current-report request failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskFail {
    /// `IOCTL_HID_GET_INPUT_REPORT` / `GetLastError` returned a Win32 error.
    Win32 { code: i32, message: String },
    /// The request was cancelled after [`ASK_TIMEOUT_MS`].
    Timeout,
    /// An answer came back, but it wasn't a full input report.
    BadReport {
        len: usize,
        head: [u8; 8],
        head_n: usize,
    },
    /// The answer failed the Bluetooth CRC.
    Crc,
    /// The request was not sent (wrong model, or no report id).
    NotAttempted,
}

impl AskFail {
    pub fn from_io(e: &io::Error) -> AskFail {
        let code = e.raw_os_error().unwrap_or(0);
        // `io::Error` Display ends with " (os error N)"; the line already
        // carries the code, so drop that suffix.
        let message = {
            let s = e.to_string();
            match s.rfind(" (os error ") {
                Some(i) => s[..i].to_string(),
                None => s,
            }
        };
        AskFail::Win32 { code, message }
    }

    pub fn bad_report(buf: &[u8]) -> AskFail {
        let n = buf.len().min(8);
        let mut head = [0u8; 8];
        head[..n].copy_from_slice(&buf[..n]);
        AskFail::BadReport {
            len: buf.len(),
            head,
            head_n: n,
        }
    }

    fn reason(&self) -> String {
        match self {
            AskFail::Win32 { code, message } => {
                format!("Win32 error {code} ({message})")
            }
            AskFail::Timeout => "timed out".to_string(),
            AskFail::BadReport { len, head, head_n } => {
                if *head_n == 0 {
                    "short or wrong report (0 bytes)".to_string()
                } else {
                    let mut hex = String::with_capacity(head_n * 3);
                    for (i, b) in head[..*head_n].iter().enumerate() {
                        if i > 0 {
                            hex.push(' ');
                        }
                        hex.push_str(&format!("{b:02X}"));
                    }
                    format!("short or wrong report ({len} bytes, first {hex})")
                }
            }
            AskFail::Crc => "CRC check failed".to_string(),
            AskFail::NotAttempted => "unsupported or not attempted".to_string(),
        }
    }

    /// The always-on log line: connection, report id, and reason.
    pub fn line(&self, model: Model, conn: Connection, id: Option<u8>) -> String {
        let over = match conn {
            Connection::Usb => "USB",
            Connection::Bluetooth => "Bluetooth",
        };
        let reason = self.reason();
        match id {
            Some(id) => format!(
                "{} current-report request failed over {over} (report 0x{id:02X}): {reason}",
                model.name()
            ),
            None => format!(
                "{} current-report request failed over {over}: {reason}",
                model.name()
            ),
        }
    }
}

/// `a` came after `b` on the controller's sensor clock, which wraps.
fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

pub struct Stall {
    model: Model,
    conn: Connection,
    id: Option<u8>,
    /// An ask failed during this silence: don't ask again until the stream
    /// delivers.
    failed: bool,
    /// State is coming from asks; stream reports must be newer than
    /// `mark` to count.
    asking: bool,
    mark: u32,
    stale_since: Option<Instant>,
}

impl Stall {
    pub fn new(model: Model, conn: Connection) -> Stall {
        Stall {
            model,
            conn,
            id: report_id(model, conn),
            failed: false,
            asking: false,
            mark: 0,
            stale_since: None,
        }
    }

    /// How long the next stream read waits.
    pub fn read_ms(&self) -> u32 {
        match (self.id, self.failed, self.asking) {
            (None, _, _) | (_, true, _) => READ_MS,
            (_, false, false) => WATCH_MS,
            (_, false, true) => ASK_GAP_MS,
        }
    }

    /// The report to ask for if the next read times out: only for DualSense
    /// and Edge, once full reports have come in (`full`), and the stream
    /// has been silent since `last_state` for [`STALL`].
    pub fn ask(&self, now: Instant, last_state: Instant, full: bool) -> Option<u8> {
        if self.failed || !full || now.duration_since(last_state) < STALL {
            return None;
        }
        self.id
    }

    /// An ask got no answer or no input state, and returns the log line
    /// for the first failure in this silence. Later calls return None so
    /// one Game Bar open is one line, not a stream. With no report id
    /// (DualShock 4) the reason is always that the request was not attempted.
    pub fn fail(&mut self, reason: AskFail) -> Option<String> {
        if self.failed {
            return None;
        }
        self.failed = true;
        let reason = if self.id.is_none() {
            AskFail::NotAttempted
        } else {
            reason
        };
        Some(reason.line(self.model, self.conn, self.id))
    }

    /// A state arrived, from an ask (`asked`) or the stream, stamped
    /// `ts` by the controller. False: it is not newer than one already
    /// used, so drop it.
    pub fn accept(&mut self, asked: bool, ts: u32, now: Instant) -> bool {
        if self.id.is_none() {
            return true;
        }
        if asked {
            if self.asking && !newer(ts, self.mark) {
                return false;
            }
            self.asking = true;
            self.mark = ts;
            return true;
        }
        self.failed = false;
        if !self.asking {
            return true;
        }
        let since = *self.stale_since.get_or_insert(now);
        if newer(ts, self.mark) || now.duration_since(since) >= STALE_FOR {
            self.asking = false;
            self.stale_since = None;
            return true;
        }
        false
    }

    /// State is coming from asks right now.
    pub fn asking(&self) -> bool {
        self.asking
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn dualshock4_is_the_old_reader() {
        let t0 = Instant::now();
        for conn in [Connection::Usb, Connection::Bluetooth] {
            let mut s = Stall::new(Model::DualShock4, conn);
            assert_eq!(s.read_ms(), 100);
            // Never asks, however long the silence.
            assert_eq!(s.ask(t0 + 60_000 * MS, t0, true), None);
            // Every report is used as it comes, in any order.
            for ts in [5, 3, 3, 1, u32::MAX, 0] {
                assert!(s.accept(false, ts, t0));
            }
            assert_eq!(s.read_ms(), 100);
            assert!(!s.asking());
        }
    }

    #[test]
    fn flowing_stream_is_not_asked() {
        let t0 = Instant::now();
        let mut s = Stall::new(Model::DualSense, Connection::Bluetooth);
        assert_eq!(s.read_ms(), WATCH_MS);
        assert!(s.accept(false, 10, t0));
        assert!(s.accept(false, 11, t0 + 4 * MS));
        // Still inside the stall window after the last state: no ask.
        assert_eq!(s.ask(t0 + 4 * MS + 39 * MS, t0 + 4 * MS, true), None);
        assert!(!s.asking());
        assert_eq!(s.read_ms(), WATCH_MS);
    }

    #[test]
    fn asks_only_after_a_silence_and_for_the_stream_report() {
        let t0 = Instant::now();
        let s = Stall::new(Model::DualSense, Connection::Bluetooth);
        assert_eq!(s.read_ms(), WATCH_MS);
        assert_eq!(s.ask(t0 + 39 * MS, t0, true), None);
        assert_eq!(s.ask(t0 + 40 * MS, t0, true), Some(0x31));
        // Not before full reports came in.
        assert_eq!(s.ask(t0 + 40 * MS, t0, false), None);
        let s = Stall::new(Model::DualSenseEdge, Connection::Usb);
        assert_eq!(s.ask(t0 + 40 * MS, t0, true), Some(0x01));
        let s = Stall::new(Model::DualShock4, Connection::Bluetooth);
        assert_eq!(s.ask(t0 + 400 * MS, t0, true), None);
    }

    #[test]
    fn a_failed_ask_waits_for_the_stream() {
        let t0 = Instant::now();
        let mut s = Stall::new(Model::DualSense, Connection::Bluetooth);
        assert!(s.fail(AskFail::Timeout).is_some());
        assert_eq!(s.read_ms(), READ_MS);
        assert_eq!(s.ask(t0 + 5_000 * MS, t0, true), None);
        assert!(s.accept(false, 10, t0));
        assert_eq!(s.read_ms(), WATCH_MS);
        assert_eq!(s.ask(t0 + 40 * MS, t0, true), Some(0x31));
    }

    #[test]
    fn asked_state_must_move_on() {
        let t0 = Instant::now();
        let mut s = Stall::new(Model::DualSense, Connection::Bluetooth);
        assert!(s.accept(true, 100, t0));
        assert!(s.asking());
        assert_eq!(s.read_ms(), ASK_GAP_MS);
        assert!(!s.accept(true, 100, t0), "the same report twice");
        assert!(s.accept(true, 200, t0));
    }

    #[test]
    fn queued_reports_from_before_are_dropped() {
        let t0 = Instant::now();
        let mut s = Stall::new(Model::DualSense, Connection::Bluetooth);
        assert!(s.accept(true, 1_000, t0));
        assert!(!s.accept(false, 900, t0));
        assert!(!s.accept(false, 1_000, t0 + MS));
        assert!(s.accept(false, 1_001, t0 + 2 * MS));
        assert!(!s.asking());
        assert_eq!(s.read_ms(), WATCH_MS);
        // Back on the stream: order no longer matters.
        assert!(s.accept(false, 5, t0 + 3 * MS));
    }

    #[test]
    fn a_jumped_clock_is_read_after_a_while() {
        let t0 = Instant::now();
        let mut s = Stall::new(Model::DualSense, Connection::Bluetooth);
        assert!(s.accept(true, 1_000, t0));
        assert!(!s.accept(false, 7, t0));
        assert!(!s.accept(false, 8, t0 + 249 * MS));
        assert!(s.accept(false, 9, t0 + 250 * MS));
    }

    #[test]
    fn the_sensor_clock_wraps() {
        let t0 = Instant::now();
        let mut s = Stall::new(Model::DualSense, Connection::Bluetooth);
        assert!(s.accept(true, u32::MAX - 5, t0));
        assert!(s.accept(true, 3, t0));
        assert!(s.accept(false, 10, t0));
    }

    #[test]
    fn ask_fail_lines() {
        assert_eq!(
            AskFail::Win32 {
                code: 1,
                message: "Incorrect function.".into(),
            }
            .line(Model::DualSense, Connection::Usb, Some(0x01)),
            "DualSense current-report request failed over USB (report 0x01): Win32 error 1 (Incorrect function.)"
        );
        assert_eq!(
            AskFail::Timeout.line(Model::DualSense, Connection::Bluetooth, Some(0x31)),
            "DualSense current-report request failed over Bluetooth (report 0x31): timed out"
        );
        assert_eq!(
            AskFail::bad_report(&[0x01, 0x00, 0x00, 0x00]).line(
                Model::DualSenseEdge,
                Connection::Usb,
                Some(0x01)
            ),
            "DualSense Edge current-report request failed over USB (report 0x01): short or wrong report (4 bytes, first 01 00 00 00)"
        );
        assert_eq!(
            AskFail::bad_report(&[]).line(Model::DualSense, Connection::Usb, Some(0x01)),
            "DualSense current-report request failed over USB (report 0x01): short or wrong report (0 bytes)"
        );
        assert_eq!(
            AskFail::Crc.line(Model::DualSense, Connection::Bluetooth, Some(0x31)),
            "DualSense current-report request failed over Bluetooth (report 0x31): CRC check failed"
        );
        assert_eq!(
            AskFail::NotAttempted.line(Model::DualShock4, Connection::Usb, None),
            "DualShock 4 current-report request failed over USB: unsupported or not attempted"
        );
    }

    #[test]
    fn win32_error_line_uses_the_os_message() {
        let e = io::Error::from_raw_os_error(1);
        let line = AskFail::from_io(&e).line(Model::DualSense, Connection::Usb, Some(0x01));
        assert!(
            line.starts_with(
                "DualSense current-report request failed over USB (report 0x01): Win32 error 1 ("
            ),
            "{line}"
        );
        assert!(line.ends_with(')'), "{line}");
        assert!(!line.contains("os error"), "{line}");
    }

    #[test]
    fn dualshock4_fail_is_not_attempted() {
        let mut s = Stall::new(Model::DualShock4, Connection::Usb);
        assert_eq!(
            s.fail(AskFail::Timeout).as_deref(),
            Some("DualShock 4 current-report request failed over USB: unsupported or not attempted")
        );
    }

    #[test]
    fn a_failed_ask_is_logged_once_per_silence() {
        let t0 = Instant::now();
        let mut s = Stall::new(Model::DualSense, Connection::Usb);
        let first = s.fail(AskFail::Timeout).expect("first failure is logged");
        assert_eq!(
            first,
            "DualSense current-report request failed over USB (report 0x01): timed out"
        );
        assert!(s
            .fail(AskFail::from_io(&io::Error::from_raw_os_error(1)))
            .is_none());
        assert_eq!(s.read_ms(), READ_MS);
        assert_eq!(s.ask(t0 + 5_000 * MS, t0, true), None);
        // The stream coming back starts a new silence.
        assert!(s.accept(false, 10, t0));
        assert_eq!(
            s.fail(AskFail::Crc).as_deref(),
            Some(
                "DualSense current-report request failed over USB (report 0x01): CRC check failed"
            )
        );
    }
}
