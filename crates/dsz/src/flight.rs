//! Flight recorder: the last couple of seconds of radio traffic with one
//! controller, written to the log only when the controller drops the link
//! on its own. Nothing is logged while things work.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// How far back the dump reaches.
const KEEP: Duration = Duration::from_secs(2);
/// Bytes kept of a control report (the whole common block on Bluetooth).
const CONTROL_BYTES: usize = 48;
/// Bytes kept of a stream report (its headers).
const STREAM_BYTES: usize = 16;
/// Input arriving later than this after the previous report is noted.
pub const INPUT_GAP: Duration = Duration::from_millis(20);

enum What {
    /// A report written: whether it went through and how long it took.
    Out {
        stream: bool,
        ok: bool,
        us: u32,
        len: usize,
        head: Vec<u8>,
    },
    /// No input for this long before the report that just came in.
    InputGap(Duration),
    /// The input switched to another report kind (0 = controller state,
    /// else the type nibble of what the controller sends instead).
    InputKind(u8),
    Note(String),
}

pub struct Recorder {
    log: Mutex<VecDeque<(Instant, What)>>,
    /// DSZ itself is turning the controller off (idle or a mapping).
    pub expect_off: AtomicBool,
    /// A read or write failed because the controller went away.
    pub lost: AtomicBool,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder {
            log: Mutex::new(VecDeque::with_capacity(512)),
            expect_off: AtomicBool::new(false),
            lost: AtomicBool::new(false),
        }
    }
}

impl Recorder {
    fn push(&self, what: What) {
        let now = Instant::now();
        let mut log = self.log.lock();
        while log
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > KEEP)
        {
            log.pop_front();
        }
        log.push_back((now, what));
    }

    pub fn write(&self, data: &[u8], stream: bool, ok: bool, took: Duration) {
        let n = if stream { STREAM_BYTES } else { CONTROL_BYTES };
        self.push(What::Out {
            stream,
            ok,
            us: took.as_micros().min(u32::MAX as u128) as u32,
            len: data.len(),
            head: data[..data.len().min(n)].to_vec(),
        });
    }

    pub fn input_gap(&self, gap: Duration) {
        self.push(What::InputGap(gap));
    }

    pub fn input_kind(&self, kind: u8) {
        self.push(What::InputKind(kind));
    }

    pub fn note(&self, s: impl Into<String>) {
        self.push(What::Note(s.into()));
    }

    /// Log the recording if the controller dropped the link unasked.
    pub fn dump_if_lost(&self, name: &str, battery: u8) {
        if !self.lost.load(Ordering::Acquire) || self.expect_off.load(Ordering::Acquire) {
            return;
        }
        let log = self.log.lock();
        let Some(end) = log.back().map(|e| e.0) else {
            return;
        };
        let mut s = format!(
            "{name} dropped the link by itself (battery {battery} %); its last {} ms of traffic, newest last:",
            KEEP.as_millis()
        );
        for (t, what) in log.iter() {
            let ago = end.duration_since(*t).as_millis();
            let _ = write!(s, "\n  -{ago:>4} ms  ");
            match what {
                What::Out {
                    stream,
                    ok,
                    us,
                    len,
                    head,
                } => {
                    let _ = write!(
                        s,
                        "{} {:02X} len {len} {} in {us} us:",
                        if *stream { "stream " } else { "control" },
                        head.first().copied().unwrap_or(0),
                        if *ok { "ok    " } else { "FAILED" },
                    );
                    for b in head {
                        let _ = write!(s, " {b:02X}");
                    }
                }
                What::InputGap(d) => {
                    let _ = write!(s, "input  gap of {} ms", d.as_millis());
                }
                What::InputKind(0) => s.push_str("input  back to controller state"),
                What::InputKind(k) => {
                    let _ = write!(s, "input  now type {k} packets");
                }
                What::Note(n) => s.push_str(n),
            }
        }
        log::warn!("{s}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_only_an_unasked_drop() {
        let r = Recorder::default();
        r.write(&[0x31, 0x10, 0x02], false, true, Duration::from_micros(800));
        r.input_gap(Duration::from_millis(30));
        // Not lost: nothing to say (the call must not panic either way).
        r.dump_if_lost("pad", 50);
        r.lost.store(true, Ordering::Release);
        r.expect_off.store(true, Ordering::Release);
        r.dump_if_lost("pad", 50);
        assert_eq!(r.log.lock().len(), 2);
    }

    #[test]
    fn keeps_only_the_head_of_a_report() {
        let r = Recorder::default();
        r.write(&[0x39; 400], true, true, Duration::ZERO);
        r.write(&[0x31; 78], false, true, Duration::ZERO);
        let log = r.log.lock();
        let lens: Vec<usize> = log
            .iter()
            .map(|(_, w)| match w {
                What::Out { head, .. } => head.len(),
                _ => 0,
            })
            .collect();
        assert_eq!(lens, vec![STREAM_BYTES, CONTROL_BYTES]);
    }
}
