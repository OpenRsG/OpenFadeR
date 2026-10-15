//! Art-Net 4 `ArtDmx` packet (UDP port 6454). Layout per the Art-Net specification: `"Art-Net\0"`, OpCode 0x5000 (little
//! endian), ProtVer 14 (big endian), Sequence, Physical, SubUni (universe low byte), Net (universe bits 8 to 14), Length
//! (big endian, even, 2 to 512), then the data.

use super::SLOTS;

pub const PORT: u16 = 6454;
const OP_DMX: u16 = 0x5000;

/// `ArtDmx` for `universe` (15-bit port-address) with `data` (the length is padded to an even number of slots).
/// `seq` 1 to 255 lets receivers drop reordered packets; 0 disables that.
pub fn packet(seq: u8, universe: u16, data: &[u8]) -> Vec<u8> {
    let n = (data.len().min(SLOTS) + 1) & !1;
    let mut p = Vec::with_capacity(18 + n);
    p.extend_from_slice(b"Art-Net\0");
    p.extend_from_slice(&OP_DMX.to_le_bytes());
    p.extend_from_slice(&[
        0,
        14,
        seq,
        0,
        (universe & 0xff) as u8,
        ((universe >> 8) & 0x7f) as u8,
    ]);
    p.extend_from_slice(&(n as u16).to_be_bytes());
    p.extend_from_slice(&data[..data.len().min(SLOTS)]);
    p.resize(18 + n, 0);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_universe_header_and_payload() {
        let data: Vec<u8> = (0..512).map(|i| i as u8).collect();
        let p = packet(7, 0x0123, &data);
        assert_eq!(p.len(), 18 + 512);
        assert_eq!(&p[..8], b"Art-Net\0");
        assert_eq!(p[8..10], [0x00, 0x50], "OpDmx 0x5000, little endian");
        assert_eq!(p[10..12], [0, 14], "protocol version 14");
        assert_eq!((p[12], p[13]), (7, 0), "sequence, physical");
        assert_eq!(
            (p[14], p[15]),
            (0x23, 0x01),
            "SubUni = low byte, Net = next 7 bits"
        );
        assert_eq!(p[16..18], [0x02, 0x00], "length 512, big endian");
        assert_eq!(p[18..], data[..]);
    }

    #[test]
    fn odd_lengths_pad_and_net_is_seven_bits() {
        let p = packet(0, 0xffff, &[1, 2, 3]);
        assert_eq!(p[16..], [0, 4, 1, 2, 3, 0]);
        assert_eq!(p[15], 0x7f);
    }
}
