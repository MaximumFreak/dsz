//! USB/IP wire format (Linux `Documentation/usb/usbip_protocol.rst`): the
//! import handshake and the 48-byte URB headers. Everything is big-endian.

use std::io::{self, Read};

pub const VERSION: u16 = 0x0111;
pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 0x0003;
pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 0x0005;

pub const CMD_SUBMIT: u32 = 1;
pub const CMD_UNLINK: u32 = 2;
pub const RET_SUBMIT: u32 = 3;
pub const RET_UNLINK: u32 = 4;

pub const DIR_OUT: u32 = 0;
pub const DIR_IN: u32 = 1;

pub const HEADER: usize = 48;
pub const ISO_DESC: usize = 16;

/// Linux errno values the client maps to USBD status codes.
pub const EPIPE: i32 = -32;
pub const ECONNRESET: i32 = -104;

/// What the USB/IP server says about the exported device.
pub struct DeviceInfo<'a> {
    pub busid: &'a str,
    pub busnum: u32,
    pub devnum: u32,
    pub speed: u32,
    pub vid: u16,
    pub pid: u16,
    pub bcd_device: u16,
    pub num_interfaces: u8,
}

fn put_str(out: &mut Vec<u8>, s: &str, len: usize) {
    let b = s.as_bytes();
    let n = b.len().min(len - 1);
    out.extend_from_slice(&b[..n]);
    out.resize(out.len() + len - n, 0);
}

/// `struct usbip_usb_device` (312 bytes).
pub fn usb_device(d: &DeviceInfo) -> Vec<u8> {
    let mut v = Vec::with_capacity(312);
    put_str(
        &mut v,
        &format!("/sys/devices/dsz/{}", d.busid),
        256,
    );
    put_str(&mut v, d.busid, 32);
    v.extend_from_slice(&d.busnum.to_be_bytes());
    v.extend_from_slice(&d.devnum.to_be_bytes());
    v.extend_from_slice(&d.speed.to_be_bytes());
    v.extend_from_slice(&d.vid.to_be_bytes());
    v.extend_from_slice(&d.pid.to_be_bytes());
    v.extend_from_slice(&d.bcd_device.to_be_bytes());
    v.extend_from_slice(&[0, 0, 0]); // class, subclass, protocol: per interface
    v.push(1); // bConfigurationValue
    v.push(1); // bNumConfigurations
    v.push(d.num_interfaces);
    v
}

pub fn op_common(code: u16, status: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[..2].copy_from_slice(&VERSION.to_be_bytes());
    b[2..4].copy_from_slice(&code.to_be_bytes());
    b[4..].copy_from_slice(&status.to_be_bytes());
    b
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// One parsed client command.
#[derive(Debug)]
pub enum Command {
    Submit(Submit),
    Unlink { seq: u32, victim: u32 },
}

#[derive(Debug, Clone)]
pub struct Submit {
    pub seq: u32,
    pub dir: u32,
    pub ep: u32,
    pub length: u32,
    pub packets: Option<u32>,
    pub setup: [u8; 8],
    /// OUT payload.
    pub data: Vec<u8>,
    /// Per-packet buffer lengths for isochronous transfers.
    pub iso_lens: Vec<u32>,
}

/// Read one command. `Ok(None)` at a clean end of stream.
pub fn read_command(r: &mut impl Read, hdr: &mut [u8; HEADER]) -> io::Result<Option<Command>> {
    match r.read_exact(hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let cmd = be32(hdr, 0);
    let seq = be32(hdr, 4);
    match cmd {
        CMD_SUBMIT => {
            let dir = be32(hdr, 12);
            let ep = be32(hdr, 16);
            let length = be32(hdr, 24);
            let np = be32(hdr, 32) as i32;
            let mut setup = [0u8; 8];
            setup.copy_from_slice(&hdr[40..48]);
            if length > 1 << 20 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "USB/IP transfer too large",
                ));
            }
            let mut data = Vec::new();
            if dir == DIR_OUT && length > 0 {
                data.resize(length as usize, 0);
                r.read_exact(&mut data)?;
            }
            let packets = (np > 0).then_some(np as u32);
            let mut iso_lens = Vec::new();
            if let Some(n) = packets {
                if n > 1024 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "too many ISO packets",
                    ));
                }
                let mut d = vec![0u8; n as usize * ISO_DESC];
                r.read_exact(&mut d)?;
                iso_lens = d
                    .as_chunks::<ISO_DESC>()
                    .0
                    .iter()
                    .map(|c| be32(c, 4))
                    .collect();
            }
            Ok(Some(Command::Submit(Submit {
                seq,
                dir,
                ep,
                length,
                packets,
                setup,
                data,
                iso_lens,
            })))
        }
        CMD_UNLINK => Ok(Some(Command::Unlink {
            seq,
            victim: be32(hdr, 20),
        })),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown USB/IP command {other}"),
        )),
    }
}

/// `RET_SUBMIT` header plus IN data (and ISO descriptors for isochronous).
pub fn ret_submit(
    out: &mut Vec<u8>,
    seq: u32,
    status: i32,
    actual: u32,
    start_frame: u32,
    iso: Option<&[(u32, u32, u32)]>,
    data: &[u8],
) {
    out.clear();
    out.extend_from_slice(&RET_SUBMIT.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&[0u8; 12]); // devid, direction, ep
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&actual.to_be_bytes());
    out.extend_from_slice(&start_frame.to_be_bytes());
    let np: i32 = iso.map(|d| d.len() as i32).unwrap_or(-1);
    out.extend_from_slice(&np.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // error_count
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(data);
    if let Some(descs) = iso {
        for &(offset, length, actual) in descs {
            out.extend_from_slice(&offset.to_be_bytes());
            out.extend_from_slice(&length.to_be_bytes());
            out.extend_from_slice(&actual.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
        }
    }
}

pub fn ret_unlink(seq: u32, status: i32) -> [u8; HEADER] {
    let mut b = [0u8; HEADER];
    b[..4].copy_from_slice(&RET_UNLINK.to_be_bytes());
    b[4..8].copy_from_slice(&seq.to_be_bytes());
    b[20..24].copy_from_slice(&status.to_be_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_record_is_312_bytes() {
        let d = DeviceInfo {
            busid: "1-1",
            busnum: 1,
            devnum: 1,
            speed: 3,
            vid: 0x054C,
            pid: 0x0CE6,
            bcd_device: 0x0100,
            num_interfaces: 4,
        };
        let v = usb_device(&d);
        assert_eq!(v.len(), 312);
        assert_eq!(&v[256..259], b"1-1");
        assert_eq!(&v[300..302], &[0x05, 0x4C]);
    }

    #[test]
    fn parses_iso_out_submit() {
        let mut m = Vec::new();
        for v in [CMD_SUBMIT, 7, 0x10001, DIR_OUT, 1, 0, 8, 0, 2, 1] {
            m.extend_from_slice(&v.to_be_bytes());
        }
        m.extend_from_slice(&[0u8; 8]);
        m.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        for (o, l) in [(0u32, 4u32), (4, 4)] {
            for v in [o, l, 0, 0] {
                m.extend_from_slice(&v.to_be_bytes());
            }
        }
        let mut hdr = [0u8; HEADER];
        let c = read_command(&mut &m[..], &mut hdr).unwrap().unwrap();
        let Command::Submit(s) = c else { panic!() };
        assert_eq!(s.seq, 7);
        assert_eq!(s.ep, 1);
        assert_eq!(s.data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(s.iso_lens, vec![4, 4]);
    }

    #[test]
    fn ret_submit_layout() {
        let mut out = Vec::new();
        ret_submit(&mut out, 9, 0, 64, 0, None, &[0xAA; 64]);
        assert_eq!(out.len(), HEADER + 64);
        assert_eq!(&out[32..36], &(-1i32).to_be_bytes());
        ret_submit(&mut out, 9, 0, 8, 5, Some(&[(0, 4, 4), (4, 4, 4)]), &[]);
        assert_eq!(out.len(), HEADER + 32);
    }
}
