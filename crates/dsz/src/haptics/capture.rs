//! Bluetooth microphone ingestion. The HID reader only validates and queues;
//! Opus decoding and delivery happen on this device's capture worker.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::Mutex;

use super::mic::{Packet, MIC_PACKET};
use super::opus::Decoder;

/// Destination for interleaved stereo, signed 16-bit, 48 kHz PCM.
/// Implementations must keep `push` and `clear` short and nonblocking.
pub trait PcmSink: Send + Sync {
    fn push(&self, pcm: &[i16]);
    fn clear(&self);
}

struct Queued {
    epoch: u64,
    packet: Packet,
}

struct Config {
    enabled: bool,
    muted: bool,
    gain: u16,
    sink: Option<Arc<dyn PcmSink>>,
}

pub struct Capture {
    config: Mutex<Config>,
    /// Epoch in the high bits; low bit is admission to the raw queue.
    admission: AtomicU64,
    tx: Sender<Queued>,
    rx: Receiver<Queued>,
    level: AtomicU8,
    pub packets: AtomicU64,
    pub dropped: AtomicU64,
    pub decode_errors: AtomicU64,
}

impl Default for Capture {
    fn default() -> Self {
        // At most 80 ms of encoded input, regardless of worker/host stalls.
        let (tx, rx) = bounded(8);
        Self {
            config: Mutex::new(Config {
                enabled: false,
                muted: false,
                gain: 100,
                sink: None,
            }),
            admission: AtomicU64::new(0),
            tx,
            rx,
            level: AtomicU8::new(0),
            packets: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            decode_errors: AtomicU64::new(0),
        }
    }
}

impl Capture {
    /// Returns whether configuration changed. Muting/disable clears the
    /// destination while holding the same lock used for delivery, so a late
    /// decoder completion cannot publish audio after a privacy transition.
    pub fn configure(&self, enabled: bool, muted: bool, gain: u16) -> bool {
        let gain = gain.min(200);
        let mut cfg = self.config.lock();
        if (cfg.enabled, cfg.muted, cfg.gain) == (enabled, muted, gain) {
            return false;
        }
        cfg.enabled = enabled;
        cfg.muted = muted;
        cfg.gain = gain;
        self.invalidate(&cfg);
        true
    }

    fn invalidate(&self, cfg: &Config) {
        let epoch = (self.admission.load(Ordering::Relaxed) & !1).wrapping_add(2);
        self.admission.store(
            epoch | u64::from(cfg.enabled && !cfg.muted),
            Ordering::Release,
        );
        self.level.store(0, Ordering::Relaxed);
        while self.rx.try_recv().is_ok() {}
        if let Some(sink) = &cfg.sink {
            sink.clear();
        }
    }

    pub fn set_sink(&self, sink: Option<Arc<dyn PcmSink>>) {
        let mut cfg = self.config.lock();
        if let Some(old) = cfg.sink.take() {
            old.clear();
        }
        cfg.sink = sink;
        self.invalidate(&cfg);
    }

    pub fn clear(&self) {
        self.invalidate(&self.config.lock());
    }
    pub fn enabled(&self) -> bool {
        self.config.lock().enabled
    }
    pub fn muted(&self) -> bool {
        self.config.lock().muted
    }
    pub fn recording(&self) -> bool {
        self.admission.load(Ordering::Acquire) & 1 != 0
    }
    pub fn peak(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }

    /// Validate the full physical report even if input CRC checking is off.
    /// HID's report id is byte 0 (the Bluetooth A1 transport byte is absent).
    pub fn packet(report: &[u8]) -> Option<Packet> {
        if report.len() < ds_proto::BT_INPUT_LEN {
            return None;
        }
        let report = &report[..ds_proto::BT_INPUT_LEN];
        if report[0] != 0x31
            || report[1] & 0x0f != 2
            || !ds_proto::crc::verify(ds_proto::crc::PREFIX_INPUT, report)
        {
            return None;
        }
        let mut data = [0; MIC_PACKET];
        data.copy_from_slice(&report[3..3 + MIC_PACKET]);
        Some((report[2], data))
    }

    pub fn enqueue(&self, packet: Packet) {
        let epoch = self.admission.load(Ordering::Acquire);
        if epoch & 1 == 0 {
            return;
        }
        if self.tx.try_send(Queued { epoch, packet }).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn deliver(&self, epoch: u64, pcm: &mut [i16]) {
        let cfg = self.config.lock();
        if self.admission.load(Ordering::Acquire) != epoch || !cfg.enabled || cfg.muted {
            return;
        }
        let mut peak = 0u32;
        for sample in pcm.iter_mut() {
            *sample = ((*sample as i32 * cfg.gain as i32) / 100)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            peak = peak.max((*sample as i32).unsigned_abs());
        }
        self.level
            .store((peak * 255 / 32768).min(255) as u8, Ordering::Relaxed);
        if let Some(sink) = &cfg.sink {
            sink.push(pcm);
        }
    }

    pub fn run(&self, stop: &std::sync::atomic::AtomicBool) {
        let mut decoder: Option<Decoder> = None;
        let mut epoch = 0;
        let mut previous = None;
        let mut pcm = [0i16; 480 * 2];
        while !stop.load(Ordering::Acquire) {
            let Ok(queued) = self.rx.recv_timeout(Duration::from_millis(50)) else {
                self.level.store(0, Ordering::Relaxed);
                continue;
            };
            if queued.epoch != self.admission.load(Ordering::Acquire) {
                continue;
            }
            if epoch != queued.epoch {
                epoch = queued.epoch;
                decoder = Decoder::new();
                previous = None;
            }
            let Some(dec) = decoder.as_mut() else {
                self.decode_errors.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let (counter, data) = queued.packet;
            if let Some(prev) = previous {
                let distance = counter.wrapping_sub(prev);
                // Ignore duplicates and late packets. Conceal a bounded loss
                // with Opus PLC, rather than stretching old audio indefinitely.
                if distance == 0 || distance > 128 {
                    continue;
                }
                for _ in 1..distance.min(4) {
                    let n = dec.conceal(&mut pcm);
                    self.deliver(epoch, &mut pcm[..n * 2]);
                }
            }
            previous = Some(counter);
            let n = dec.decode(&data, &mut pcm);
            if n == 0 {
                self.decode_errors.fetch_add(1, Ordering::Relaxed);
            } else {
                self.packets.fetch_add(1, Ordering::Relaxed);
                self.deliver(epoch, &mut pcm[..n * 2]);
            }
        }
        self.configure(false, true, 100);
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Sink(Mutex<Vec<i16>>);
    impl PcmSink for Sink {
        fn push(&self, pcm: &[i16]) {
            self.0.lock().extend_from_slice(pcm);
        }
        fn clear(&self) {
            self.0.lock().clear();
        }
    }

    #[test]
    fn validates_type_length_and_crc_even_without_input_crc_setting() {
        let mut r = [0; ds_proto::BT_INPUT_LEN];
        r[0] = 0x31;
        r[1] = 0xa2;
        r[2] = 255;
        r[3] = 0xf0;
        ds_proto::crc::seal(ds_proto::crc::PREFIX_INPUT, &mut r);
        assert_eq!(Capture::packet(&r).unwrap().0, 255);
        assert!(Capture::packet(&r[..77]).is_none());
        r[4] ^= 1;
        assert!(Capture::packet(&r).is_none());
        r[1] = 1;
        ds_proto::crc::seal(ds_proto::crc::PREFIX_INPUT, &mut r);
        assert!(Capture::packet(&r).is_none());
    }

    #[test]
    fn mute_flushes_sink_and_rejects_old_epoch_after_unmute() {
        let c = Capture::default();
        let sink = Arc::new(Sink::default());
        c.set_sink(Some(sink.clone()));
        c.configure(true, false, 200);
        let old = c.admission.load(Ordering::Acquire);
        c.deliver(old, &mut [20_000, -20_000]);
        assert_eq!(&*sink.0.lock(), &[32767, -32768]);
        c.configure(true, true, 100);
        assert!(sink.0.lock().is_empty());
        c.configure(true, false, 100);
        c.deliver(old, &mut [123, 123]);
        assert!(sink.0.lock().is_empty());
        c.configure(false, false, 100);
        assert!(!c.recording());
    }

    #[test]
    fn raw_backlog_is_bounded_and_disable_drains_it() {
        let c = Capture::default();
        c.configure(true, false, 100);
        for i in 0..100 {
            c.enqueue((i, [0; MIC_PACKET]));
        }
        assert_eq!(c.rx.len(), 8);
        assert_eq!(c.dropped.load(Ordering::Relaxed), 92);
        c.configure(false, false, 100);
        assert!(c.rx.is_empty());
    }

    #[test]
    fn captured_edge_microphone_report_decodes_through_worker_to_stereo_sink() {
        struct ChannelSink(Sender<Vec<i16>>);
        impl PcmSink for ChannelSink {
            fn push(&self, pcm: &[i16]) {
                let _ = self.0.try_send(pcm.to_vec());
            }
            fn clear(&self) {}
        }
        // Existing upstream capture, also used by ds-proto's input parser
        // regression test. This is hardware evidence, not synthetic Opus.
        let report: Vec<u8> = "31 12 00 D4 7E CE 2B EF 0E 45 AC 7B AB 01 FF DA C3 0D 66 CE 3A 3C 87 D9 95 68 7F 1F ED 42 0F B5 3B EF 67 03 CE 5E D3 8C 25 E0 C4 6F 63 3B DC C4 7C C2 F5 C1 56 7D C4 F8 AE AB A0 96 89 5B 68 94 2A DD 21 B0 13 5F 8C 3C 80 24 47 A5 39 73"
            .split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect();
        let c = Capture::default();
        let (tx, rx) = bounded(1);
        c.set_sink(Some(Arc::new(ChannelSink(tx))));
        c.configure(true, false, 100);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let decoded = std::thread::scope(|s| {
            s.spawn(|| c.run(&stop));
            c.enqueue(Capture::packet(&report).expect("valid captured packet"));
            let result = rx.recv_timeout(Duration::from_secs(2));
            stop.store(true, Ordering::Release);
            result
        })
        .expect("worker delivered captured microphone audio");
        assert_eq!(decoded.len(), 480 * 2, "10 ms of stereo 48 kHz PCM");
        assert!(decoded.iter().any(|s| *s != 0));
        assert_eq!(c.packets.load(Ordering::Relaxed), 1);
        assert_eq!(c.decode_errors.load(Ordering::Relaxed), 0);
        assert!(!c.recording(), "worker stop closes microphone admission");
    }
}
