//! Reflected CRC-32 (poly `0xEDB88320`) with the Bluetooth HID prefix byte
//! folded in first, as Linux's `hid-playstation` does: output reports use
//! prefix `0xA2`, input reports `0xA1`.

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = make_table();

pub const PREFIX_OUTPUT: u8 = 0xA2;
pub const PREFIX_INPUT: u8 = 0xA1;

#[inline]
pub fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// Standard CRC-32 of `data` with `prefix` hashed first.
#[inline]
pub fn with_prefix(prefix: u8, data: &[u8]) -> u32 {
    !update(update(0xFFFF_FFFF, &[prefix]), data)
}

/// Write the CRC of `report[..len-4]` into the last 4 bytes, little-endian.
pub fn seal(prefix: u8, report: &mut [u8]) {
    let n = report.len();
    debug_assert!(n > 4);
    let c = with_prefix(prefix, &report[..n - 4]);
    report[n - 4..].copy_from_slice(&c.to_le_bytes());
}

/// Check the trailing CRC of a Bluetooth report of `len` bytes.
pub fn verify(prefix: u8, report: &[u8]) -> bool {
    let n = report.len();
    if n <= 4 {
        return false;
    }
    let want = u32::from_le_bytes([report[n - 4], report[n - 3], report[n - 2], report[n - 1]]);
    with_prefix(prefix, &report[..n - 4]) == want
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_check_value() {
        // The CRC-32 check value: "123456789" gives CBF43926.
        assert_eq!(!update(0xFFFF_FFFF, b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn prefix_is_hashed_first() {
        let data = [0x31, 0x02, 0x10, 0x20];
        let mut joined = vec![PREFIX_OUTPUT];
        joined.extend_from_slice(&data);
        assert_eq!(with_prefix(PREFIX_OUTPUT, &data), !update(0xFFFF_FFFF, &joined));
    }
}
