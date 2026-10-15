//! UDP output of DMX universes as Art-Net and / or sACN to one unicast (or broadcast) address.

use super::{artnet, sacn, Universe};
use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    Off,
    ArtNet,
    Sacn,
    Both,
}

impl Protocol {
    pub const ALL: [Self; 4] = [Self::Off, Self::ArtNet, Self::Sacn, Self::Both];

    pub fn label(self) -> &'static str {
        ["OFF", "ART-NET", "SACN", "BOTH"][self as usize]
    }
}

/// `localhost` or an IP address.
pub fn parse_ip(s: &str) -> io::Result<IpAddr> {
    match s.trim() {
        "localhost" => Ok(Ipv4Addr::LOCALHOST.into()),
        t => t.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("\"{t}\" is not an IP address"),
            )
        }),
    }
}

pub struct Output {
    sock: UdpSocket,
    target: IpAddr,
    /// Art-Net and sACN destination ports (the standard ones outside tests).
    ports: (u16, u16),
    pub protocol: Protocol,
    seq: u8,
    cid: [u8; 16],
}

impl Output {
    pub fn new(protocol: Protocol, ip: &str) -> io::Result<Self> {
        Self::with_ports(protocol, ip, artnet::PORT, sacn::PORT)
    }

    pub fn with_ports(
        protocol: Protocol,
        ip: &str,
        artnet_port: u16,
        sacn_port: u16,
    ) -> io::Result<Self> {
        let target = parse_ip(ip)?;
        let sock = UdpSocket::bind(if target.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        })?;
        sock.set_broadcast(true)?;
        // The CID only has to differ between senders: process id and start time.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut cid = [0; 16];
        cid[..4].copy_from_slice(&std::process::id().to_be_bytes());
        cid[4..].copy_from_slice(&t.to_be_bytes()[4..]);
        Ok(Self {
            sock,
            target,
            ports: (artnet_port, sacn_port),
            protocol,
            seq: 0,
            cid,
        })
    }

    /// Sends every universe of `frames` with the selected protocols. A refused datagram (nothing listening yet) is not an error.
    pub fn send(&mut self, frames: &BTreeMap<u16, Universe>) -> io::Result<()> {
        // Art-Net reserves sequence 0 for "not sequenced".
        self.seq = if self.seq == 255 { 1 } else { self.seq + 1 };
        for (&u, data) in frames {
            let art = matches!(self.protocol, Protocol::ArtNet | Protocol::Both)
                .then(|| (artnet::packet(self.seq, u, data), self.ports.0));
            let sacn = matches!(self.protocol, Protocol::Sacn | Protocol::Both)
                .then(|| (sacn::packet(self.cid, self.seq, u, data), self.ports.1));
            for (p, port) in art.into_iter().chain(sacn) {
                match self.sock.send_to(&p, (self.target, port)) {
                    Err(e) if e.kind() != io::ErrorKind::ConnectionRefused => return Err(e),
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::{fixtures::builtin, Control, Input, Mode, Rig, SLOTS};
    use std::time::Duration;

    fn listener() -> (UdpSocket, u16) {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let port = s.local_addr().unwrap().port();
        (s, port)
    }

    /// The whole path: a patched rig, a solid red look, one UDP datagram per protocol received on localhost.
    #[test]
    fn rig_frames_reach_localhost_as_artnet_and_sacn() {
        let mut rig = Rig::default();
        rig.add(&builtin()[0]);
        let looks = rig.looks(
            &Control {
                mode: Mode::Solid([255, 0, 0]),
                ..Control::default()
            },
            &Input {
                beat: 0.0,
                bpm: 120.0,
                phrase: None,
            },
        );
        let frames = rig.frames(&looks);
        let ((art, art_port), (sacn, sacn_port)) = (listener(), listener());
        let mut out = Output::with_ports(Protocol::Both, "localhost", art_port, sacn_port).unwrap();
        out.send(&frames).unwrap();
        let mut buf = [0u8; 1024];
        let n = art.recv(&mut buf).unwrap();
        assert_eq!(&buf[..8], b"Art-Net\0");
        assert_eq!((n, buf[12]), (18 + SLOTS, 1), "first sequence number is 1");
        assert_eq!(buf[18..21], [255, 0, 0], "fixture patched at address 1");
        let n = sacn.recv(&mut buf).unwrap();
        assert_eq!((n, &buf[4..13]), (638, &b"ASC-E1.17"[..]));
        assert_eq!(buf[126..129], [255, 0, 0]);
        assert_eq!(buf[113..115], [0, 1], "universe 0 → wire universe 1");
    }

    #[test]
    fn off_sends_nothing_and_sequence_skips_zero() {
        let (l, port) = listener();
        l.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let frames = BTreeMap::from([(0u16, [0u8; SLOTS])]);
        let mut out = Output::with_ports(Protocol::Off, "127.0.0.1", port, port).unwrap();
        out.send(&frames).unwrap();
        assert!(l.recv(&mut [0; 16]).is_err());
        out.protocol = Protocol::ArtNet;
        out.seq = 255;
        out.send(&frames).unwrap();
        let mut buf = [0u8; 1024];
        l.recv(&mut buf).unwrap();
        assert_eq!(buf[12], 1);
    }

    #[test]
    fn bad_addresses_are_rejected() {
        assert!(parse_ip("not an ip").is_err());
        assert_eq!(parse_ip(" 192.0.2.9 ").unwrap().to_string(), "192.0.2.9");
    }
}
