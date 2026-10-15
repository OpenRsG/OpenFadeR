//! sACN (ANSI E1.31) data packet (UDP port 5568). Layout per E1.31: root layer (preamble 0x0010, postamble 0, ACN
//! packet identifier `"ASC-E1.17\0\0\0"`, flags+length, vector 4, 16-byte CID), framing layer (flags+length, vector 2,
//! 64-byte source name, priority, sync address, sequence, options, universe), DMP layer (flags+length, vector 2, address
//! type 0xA1, first address 0, increment 1, property count, start code 0, data). A full universe is 638 bytes.
//! Universes are 1 to 63999 on the wire; our 0-based universe `u` goes out as `u + 1`.

use super::SLOTS;

pub const PORT: u16 = 5568;
pub const PRIORITY: u8 = 100;
const SOURCE: &[u8] = b"OpenFadeR";

fn flags_len(n: usize) -> [u8; 2] {
    (0x7000 | n as u16).to_be_bytes()
}

/// Data packet for 0-based `universe` from sender `cid` with up to 512 slots.
pub fn packet(cid: [u8; 16], seq: u8, universe: u16, data: &[u8]) -> Vec<u8> {
    let data = &data[..data.len().min(SLOTS)];
    let total = 126 + data.len();
    let mut p = Vec::with_capacity(total);
    p.extend_from_slice(&[0x00, 0x10, 0x00, 0x00]);
    p.extend_from_slice(b"ASC-E1.17\0\0\0");
    p.extend_from_slice(&flags_len(total - 16));
    p.extend_from_slice(&4u32.to_be_bytes());
    p.extend_from_slice(&cid);
    p.extend_from_slice(&flags_len(total - 38));
    p.extend_from_slice(&2u32.to_be_bytes());
    let mut name = [0u8; 64];
    name[..SOURCE.len()].copy_from_slice(SOURCE);
    p.extend_from_slice(&name);
    p.extend_from_slice(&[PRIORITY, 0, 0, seq, 0]);
    p.extend_from_slice(&(universe + 1).to_be_bytes());
    p.extend_from_slice(&flags_len(total - 115));
    p.extend_from_slice(&[0x02, 0xa1, 0, 0, 0, 1]);
    p.extend_from_slice(&(data.len() as u16 + 1).to_be_bytes());
    p.push(0); // DMX start code
    p.extend_from_slice(data);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_universe_layout() {
        let cid = [9; 16];
        let data: Vec<u8> = (0..512).map(|i| (i * 3) as u8).collect();
        let p = packet(cid, 5, 2, &data);
        assert_eq!(p.len(), 638);
        assert_eq!(p[..4], [0, 0x10, 0, 0]);
        assert_eq!(&p[4..16], b"ASC-E1.17\0\0\0");
        assert_eq!(p[16..18], [0x72, 0x6e], "root flags+length 622");
        assert_eq!(p[18..22], [0, 0, 0, 4]);
        assert_eq!(p[22..38], cid);
        assert_eq!(p[38..40], [0x72, 0x58], "framing flags+length 600");
        assert_eq!(p[40..44], [0, 0, 0, 2]);
        assert_eq!(&p[44..53], b"OpenFadeR");
        assert!(p[53..108].iter().all(|&b| b == 0));
        assert_eq!((p[108], p[111]), (PRIORITY, 5), "priority, sequence");
        assert_eq!(p[113..115], [0, 3], "0-based universe 2 is wire universe 3");
        assert_eq!(p[115..117], [0x72, 0x0b], "DMP flags+length 523");
        assert_eq!(p[117..123], [0x02, 0xa1, 0, 0, 0, 1]);
        assert_eq!(p[123..125], [0x02, 0x01], "513 properties");
        assert_eq!(p[125], 0, "start code");
        assert_eq!(p[126..], data[..]);
    }

    #[test]
    fn short_payload_shrinks_every_length() {
        let p = packet([0; 16], 0, 0, &[1, 2, 3]);
        assert_eq!(p.len(), 129);
        assert_eq!(p[16..18], [0x70, 0x71]); // 129 - 16 = 113
        assert_eq!(p[38..40], [0x70, 0x5b]); // 129 - 38 = 91
        assert_eq!(p[115..117], [0x70, 0x0e]); // 129 - 115 = 14
        assert_eq!(p[123..125], [0, 4]);
    }
}
