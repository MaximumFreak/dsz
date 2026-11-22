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

/// `a` came after `b` on the controller's sensor clock, which wraps.
fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

pub struct Stall {
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

    /// An ask got no answer or no input state.
    pub fn ask_failed(&mut self) {
        self.failed = true;
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
        s.ask_failed();
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
}
