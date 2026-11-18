//! Replies read off a packet socket, as udhcpc reads them before the
//! interface has an address: whole IPv4 datagrams, from which the DHCP
//! message is taken once its headers and checksums hold up. A classic BPF
//! filter on the socket lets through only unfragmented UDP to port 68, so
//! the thread isn't woken for every other packet on the network.
use etherparse::{IpNumber, Ipv4Slice, UdpSlice};

pub const CLIENT_PORT: u16 = 68;
pub const SERVER_PORT: u16 = 67;

/// The DHCP message in an IPv4 datagram from port 67 to port 68, if its
/// headers and checksums are right. `udp_sum`: the UDP checksum is filled
/// in; a packet the kernel sent itself through a device that would have
/// filled it in (TP_STATUS_CSUMNOTREADY) arrives without it.
pub fn dhcp_payload(datagram: &[u8], udp_sum: bool) -> Option<&[u8]> {
    let ip = Ipv4Slice::from_slice(datagram).ok()?;
    let header = ip.header();
    if ip.is_payload_fragmented()
        || ip.payload_ip_number() != IpNumber::UDP
        || header.to_header().calc_header_checksum() != header.header_checksum()
    {
        return None;
    }
    let udp = UdpSlice::from_slice(ip.payload().payload).ok()?;
    if udp.source_port() != SERVER_PORT || udp.destination_port() != CLIENT_PORT {
        return None;
    }
    // 0: the sender left it out (allowed for UDP over IPv4).
    if udp_sum && udp.checksum() != 0 {
        let sum = udp
            .to_header()
            .calc_checksum_ipv4(&header.to_header(), udp.payload())
            .ok()?;
        if sum != udp.checksum() {
            return None;
        }
    }
    Some(udp.payload())
}

/// One BPF instruction: code, jump if true, jump if false, k.
pub type Instruction = (u16, u8, u8, u32);

// Classic BPF (linux/filter.h).
const LD_B_ABS: u16 = 0x30;
const LD_H_ABS: u16 = 0x28;
const LD_H_IND: u16 = 0x48;
const LDX_B_MSH: u16 = 0xb1;
const JEQ_K: u16 = 0x15;
const JSET_K: u16 = 0x45;
const RET_K: u16 = 0x06;

/// On an IPv4 datagram (a cooked packet socket's data): UDP, not a
/// fragment, to port 68; anything else is dropped in the kernel.
pub const FILTER: [Instruction; 9] = [
    // The protocol: UDP, or drop.
    (LD_B_ABS, 0, 0, 9),
    (JEQ_K, 0, 6, 17),
    // Fragment offset or more-fragments: drop.
    (LD_H_ABS, 0, 0, 6),
    (JSET_K, 4, 0, 0x3fff),
    // X = the IP header's length; the UDP destination port after it.
    (LDX_B_MSH, 0, 0, 0),
    (LD_H_IND, 0, 0, 2),
    (JEQ_K, 0, 1, CLIENT_PORT as u32),
    // Accept the whole datagram.
    (RET_K, 0, 0, 0x0004_0000),
    (RET_K, 0, 0, 0),
];

#[cfg(test)]
mod tests {
    use super::*;
    use etherparse::PacketBuilder;

    fn datagram(from: u16, to: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        PacketBuilder::ipv4([192, 0, 2, 1], [255, 255, 255, 255], 64)
            .udp(from, to)
            .write(&mut out, payload)
            .unwrap();
        out
    }

    /// Runs `FILTER` as the kernel would: the length it keeps, 0 to drop.
    fn run(program: &[Instruction], packet: &[u8]) -> u32 {
        let (mut a, mut x, mut pc) = (0u32, 0u32, 0usize);
        let byte = |at: usize| packet.get(at).copied().map(u32::from);
        let half = |at: usize| Some(byte(at)? << 8 | byte(at + 1)?);
        loop {
            let (code, jt, jf, k) = program[pc];
            pc += 1;
            let loaded = match code {
                LD_B_ABS => byte(k as usize),
                LD_H_ABS => half(k as usize),
                LD_H_IND => half((x + k) as usize),
                LDX_B_MSH => {
                    x = (byte(k as usize).unwrap_or(0) & 0xf) * 4;
                    continue;
                }
                JEQ_K | JSET_K => {
                    let taken = if code == JEQ_K { a == k } else { a & k != 0 };
                    pc += usize::from(if taken { jt } else { jf });
                    continue;
                }
                RET_K => return k,
                other => panic!("opcode {other:#x}"),
            };
            match loaded {
                Some(value) => a = value,
                // Out of bounds: the kernel drops the packet.
                None => return 0,
            }
        }
    }

    #[test]
    fn a_reply_to_port_68_is_taken_out_whole() {
        let packet = datagram(67, 68, b"DHCP");
        assert_eq!(dhcp_payload(&packet, true), Some(&b"DHCP"[..]));
        assert_ne!(run(&FILTER, &packet), 0);
    }

    #[test]
    fn other_ports_and_broken_checksums_are_ignored() {
        assert_eq!(dhcp_payload(&datagram(53, 68, b"x"), true), None);
        assert_eq!(dhcp_payload(&datagram(67, 67, b"x"), true), None);
        assert_eq!(run(&FILTER, &datagram(67, 67, b"x")), 0);
        assert_eq!(run(&FILTER, &datagram(67, 5353, b"x")), 0);
        let mut bad = datagram(67, 68, b"DHCP");
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        assert_eq!(dhcp_payload(&bad, true), None, "UDP checksum");
        // Not yet filled in: not checked.
        let mut partial = datagram(67, 68, b"DHCP");
        partial[26] ^= 0x55;
        assert_eq!(dhcp_payload(&partial, false), Some(&b"DHCP"[..]));
        let mut bad = datagram(67, 68, b"DHCP");
        bad[8] ^= 1;
        assert_eq!(dhcp_payload(&bad, false), None, "IP header checksum");
        // No UDP checksum at all is allowed.
        let mut none = datagram(67, 68, b"DHCP");
        none[26] = 0;
        none[27] = 0;
        assert_eq!(dhcp_payload(&none, true), Some(&b"DHCP"[..]));
        assert_eq!(dhcp_payload(&[0x45, 0, 0], true), None);
    }

    #[test]
    fn the_filter_drops_fragments_and_other_protocols() {
        let mut tcp = Vec::new();
        PacketBuilder::ipv4([192, 0, 2, 1], [192, 0, 2, 30], 64)
            .tcp(67, 68, 1, 1024)
            .write(&mut tcp, b"x")
            .unwrap();
        assert_eq!(run(&FILTER, &tcp), 0);
        let mut fragment = datagram(67, 68, b"DHCP");
        fragment[6] |= 0x20; // more fragments
        assert_eq!(run(&FILTER, &fragment), 0);
        assert_eq!(dhcp_payload(&fragment, true), None);
        // IP options move the UDP header; the filter follows.
        let mut options = datagram(67, 68, b"DHCP");
        options[0] = 0x46;
        options.splice(20..20, [1, 1, 1, 1]);
        assert_ne!(run(&FILTER, &options), 0);
    }
}
