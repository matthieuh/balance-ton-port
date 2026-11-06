//! Parser for macOS `sysctl net.inet.tcp.pcblist_n` (what netstat reads) and
//! the pure aggregation behind `Platform::port_pressure`. Kept free of any
//! syscall so it compiles and is tested on every OS.

use super::{PortPressure, StormTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TcpState {
    Listen,
    TimeWait,
    Other,
}

/// One TCP socket of the machine, as much as the port pressure needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sock {
    pub state: TcpState,
    pub local_port: u16,
    /// IPv4 as `::ffff:a.b.c.d`.
    pub remote_addr: [u8; 16],
    pub remote_port: u16,
    /// `xsocket_n.so_last_pid`: the last process that used the socket. Kept
    /// after the process closed it (TIME_WAIT).
    pub last_pid: i32,
}

// Record kinds (`xgn_kind`, bsd/sys/socketvar.h).
const XSO_SOCKET: u32 = 0x001;
const XSO_RCVBUF: u32 = 0x002;
const XSO_SNDBUF: u32 = 0x004;
const XSO_STATS: u32 = 0x008;
const XSO_INPCB: u32 = 0x010;
const XSO_TCPCB: u32 = 0x020;
const ALL_KINDS: u32 = XSO_SOCKET | XSO_RCVBUF | XSO_SNDBUF | XSO_STATS | XSO_INPCB | XSO_TCPCB;

/// `sizeof(struct xinpgen)`: the header and the trailer of the list.
const XINPGEN_LEN: usize = 24;

// Field offsets. The kernel lays these records out with 4-byte packing, so a
// u64 may follow a u16 or a u32 without padding (checked against
// `netstat -anv` on macOS 26).
/// `xinpcb_n.inp_fport`, `inp_lport`: u16 each, network order.
const INP_FPORT: usize = 16;
const INP_LPORT: usize = 18;
/// `xinpcb_n.inp_vflag`: `INP_IPV4` 0x1, `INP_IPV6` 0x2.
const INP_VFLAG: usize = 44;
/// `xinpcb_n.inp_dependfaddr`: an `in6_addr`, or an `in_addr_4in6` whose
/// IPv4 address is its last 4 bytes.
const INP_FADDR: usize = 48;
const INP_MIN_LEN: usize = INP_FADDR + 16;
const INP_IPV4: u8 = 0x1;
/// `xsocket_n.so_last_pid`.
const SO_LAST_PID: usize = 68;
const SO_MIN_LEN: usize = SO_LAST_PID + 4;
/// `xtcpcb_n.t_state`.
const T_STATE: usize = 36;
const T_MIN_LEN: usize = T_STATE + 4;
// `t_state` values (bsd/netinet/tcp_fsm.h).
const TCPS_LISTEN: i32 = 1;
const TCPS_TIME_WAIT: i32 = 10;

fn u32_at(buf: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(buf.get(off..off + 4)?.try_into().ok()?))
}

fn u16_be_at(buf: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(buf.get(off..off + 2)?.try_into().ok()?))
}

/// Every TCP socket of a `net.inet.tcp.pcblist_n` buffer.
///
/// The buffer is an `xinpgen` header, then for each socket six records
/// (`xinpcb_n`, `xsocket_n`, two `xsockbuf_n`, `xsockstat_n`, `xtcpcb_n`),
/// each starting with its length and kind and padded to 8 bytes, then an
/// `xinpgen` trailer. Anything else (a kind we do not know, a record too
/// short for the fields we read or running past the end, a kind twice in
/// one socket, a missing trailer, an unknown state) gives `None`: a layout
/// change must never turn into wrong numbers.
pub(crate) fn parse_pcblist_n(buf: &[u8]) -> Option<Vec<Sock>> {
    if u32_at(buf, 0)? as usize != XINPGEN_LEN {
        return None;
    }
    let mut out = Vec::new();
    let mut off = XINPGEN_LEN;
    let mut seen = 0u32;
    let (mut inp, mut so, mut tcp) = (&[][..], &[][..], &[][..]);
    loop {
        let len = u32_at(buf, off)? as usize;
        if len == XINPGEN_LEN {
            // The trailer, told apart by its length alone: no data record
            // is ever exactly 24 bytes. Only valid between two sockets, whole.
            return (seen == 0 && off + len <= buf.len()).then_some(out);
        }
        let kind = u32_at(buf, off + 4)?;
        let rec = buf.get(off..off.checked_add(len)?)?;
        let min = match kind {
            XSO_INPCB => INP_MIN_LEN,
            XSO_SOCKET => SO_MIN_LEN,
            XSO_TCPCB => T_MIN_LEN,
            XSO_RCVBUF | XSO_SNDBUF | XSO_STATS => 8,
            _ => return None,
        };
        if len < min || seen & kind != 0 {
            return None;
        }
        seen |= kind;
        match kind {
            XSO_INPCB => inp = rec,
            XSO_SOCKET => so = rec,
            XSO_TCPCB => tcp = rec,
            _ => {}
        }
        if seen == ALL_KINDS {
            out.push(sock(inp, so, tcp)?);
            seen = 0;
        }
        // ROUNDUP64, as netstat walks it.
        off = off.checked_add(len.div_ceil(8) * 8)?;
    }
}

fn sock(inp: &[u8], so: &[u8], tcp: &[u8]) -> Option<Sock> {
    let state = match u32_at(tcp, T_STATE)? as i32 {
        TCPS_LISTEN => TcpState::Listen,
        TCPS_TIME_WAIT => TcpState::TimeWait,
        0..=9 => TcpState::Other,
        _ => return None,
    };
    let faddr: [u8; 16] = inp.get(INP_FADDR..INP_FADDR + 16)?.try_into().ok()?;
    let remote_addr = if inp[INP_VFLAG] & INP_IPV4 != 0 {
        let mut a = [0u8; 16];
        a[10] = 0xff;
        a[11] = 0xff;
        a[12..].copy_from_slice(&faddr[12..]);
        a
    } else {
        faddr
    };
    Some(Sock {
        state,
        local_port: u16_be_at(inp, INP_LPORT)?,
        remote_addr,
        remote_port: u16_be_at(inp, INP_FPORT)?,
        last_pid: u32_at(so, SO_LAST_PID)? as i32,
    })
}

/// Ports in `first..=last`, 0 when inverted.
pub(crate) fn range_size(first: u16, last: u16) -> u32 {
    if last >= first {
        u32::from(last - first) + 1
    } else {
        0
    }
}

/// The pressure on the ephemeral range `first..=last`.
///
/// `in_use` counts the sockets of any state but LISTEN whose local port is
/// in the range: those are the ports a new outgoing connection cannot take.
/// The storm is the TIME_WAIT part: `top` is the remote port of the largest
/// group of those, named only when it holds at least half of `in_use`. Its
/// client is the pid holding at least 80% of the group.
pub(crate) fn aggregate(
    socks: &[Sock],
    first: u16,
    last: u16,
    is_local: impl Fn(&[u8; 16]) -> bool,
    name_of: impl Fn(i32) -> Option<String>,
) -> PortPressure {
    use std::collections::HashMap;
    let in_range: Vec<&Sock> = socks
        .iter()
        .filter(|s| s.state != TcpState::Listen && (first..=last).contains(&s.local_port))
        .collect();
    let in_use = in_range.len() as u32;
    let waiting: Vec<&Sock> = in_range
        .into_iter()
        .filter(|s| s.state == TcpState::TimeWait)
        .collect();
    let time_wait = waiting.len() as u32;

    let mut groups: HashMap<u16, u32> = HashMap::new();
    for s in &waiting {
        *groups.entry(s.remote_port).or_default() += 1;
    }
    // Largest group; the lowest port on a tie, so the answer is stable.
    let top = groups
        .into_iter()
        .max_by_key(|&(port, n)| (n, std::cmp::Reverse(port)))
        .filter(|&(_, n)| u64::from(n) * 2 >= u64::from(in_use))
        .map(|(port, n)| {
            let group = || waiting.iter().filter(move |s| s.remote_port == port);
            let mut pids: HashMap<i32, u32> = HashMap::new();
            for s in group() {
                *pids.entry(s.last_pid).or_default() += 1;
            }
            let client_pid = pids
                .into_iter()
                .find(|&(pid, c)| pid > 0 && u64::from(c) * 5 >= u64::from(n) * 4)
                .map(|(pid, _)| pid);
            StormTarget {
                port,
                local: group().all(|s| is_local(&s.remote_addr)),
                sockets: n,
                client_pid,
                client_name: client_pid.and_then(&name_of),
            }
        });
    PortPressure {
        range_size: range_size(first, last),
        in_use,
        time_wait,
        top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/pcblist_n.bin");
    /// Port of the listener the fixture's 300 client sockets connected to.
    const FIXTURE_PORT: u16 = 47123;

    fn v4(a: [u8; 4]) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[10] = 0xff;
        out[11] = 0xff;
        out[12..].copy_from_slice(&a);
        out
    }

    const LOOPBACK: [u8; 4] = [127, 0, 0, 1];

    fn sock(state: TcpState, local_port: u16, remote_port: u16, pid: i32) -> Sock {
        Sock {
            state,
            local_port,
            remote_addr: v4(LOOPBACK),
            remote_port,
            last_pid: pid,
        }
    }

    /// `n` sockets with distinct local ports of the ephemeral range.
    fn many(n: usize, start: u16, state: TcpState, remote_port: u16, pid: i32) -> Vec<Sock> {
        (0..n)
            .map(|i| sock(state, start + i as u16, remote_port, pid))
            .collect()
    }

    fn loopback(a: &[u8; 16]) -> bool {
        a[..12] == v4(LOOPBACK)[..12] && a[12] == 127
    }

    fn named(pid: i32) -> Option<String> {
        Some(format!("proc{pid}"))
    }

    #[test]
    fn parses_fixture() {
        let socks = parse_pcblist_n(FIXTURE).expect("fixture parses");
        let tw = socks
            .iter()
            .filter(|s| s.state == TcpState::TimeWait && s.remote_port == FIXTURE_PORT)
            .count();
        assert!(tw >= 300, "{tw} TIME_WAIT sockets to {FIXTURE_PORT}");
        // The listener itself is in the list too.
        assert!(socks
            .iter()
            .any(|s| s.state == TcpState::Listen && s.local_port == FIXTURE_PORT));
        assert!(socks
            .iter()
            .filter(|s| s.remote_port == FIXTURE_PORT)
            .all(|s| s.remote_addr == v4(LOOPBACK)));
    }

    #[test]
    fn truncated_buffer_is_none() {
        assert_eq!(parse_pcblist_n(&FIXTURE[..1000]), None);
        assert_eq!(parse_pcblist_n(&[]), None);
        // Cut right before the trailer: every socket is whole, the end is not.
        assert_eq!(parse_pcblist_n(&FIXTURE[..FIXTURE.len() - 24]), None);
    }

    #[test]
    fn unknown_kind_or_bad_header_is_none() {
        let mut bad = FIXTURE.to_vec();
        bad[0] = 32; // xig_len
        assert_eq!(parse_pcblist_n(&bad), None);
        let mut bad = FIXTURE.to_vec();
        bad[28] = 0x40; // first record's kind: XSO_KCREG
        assert_eq!(parse_pcblist_n(&bad), None);
    }

    #[test]
    fn header_and_trailer_only_is_empty() {
        let mut buf = FIXTURE[..24].to_vec();
        buf.extend_from_slice(&FIXTURE[FIXTURE.len() - 24..]);
        assert_eq!(parse_pcblist_n(&buf), Some(Vec::new()));
    }

    #[test]
    fn aggregate_names_majority_client() {
        let mut socks = many(900, 49152, TcpState::TimeWait, 8081, 7);
        socks.extend(many(100, 50052, TcpState::TimeWait, 8081, 8));
        let p = aggregate(&socks, 49152, 65535, loopback, named);
        assert_eq!(p.range_size, 16384);
        assert_eq!(p.in_use, 1000);
        assert_eq!(p.time_wait, 1000);
        let top = p.top.expect("top");
        assert_eq!(top.port, 8081);
        assert_eq!(top.sockets, 1000);
        assert!(top.local);
        assert_eq!(top.client_pid, Some(7));
        assert_eq!(top.client_name.as_deref(), Some("proc7"));
    }

    #[test]
    fn aggregate_no_client_without_80_percent() {
        let mut socks = many(700, 49152, TcpState::TimeWait, 8081, 7);
        socks.extend(many(300, 50052, TcpState::TimeWait, 8081, 8));
        let top = aggregate(&socks, 49152, 65535, loopback, named)
            .top
            .expect("top");
        assert_eq!(top.sockets, 1000);
        assert_eq!(top.client_pid, None);
        assert_eq!(top.client_name, None);
    }

    #[test]
    fn exactly_80_percent_is_a_client() {
        let mut socks = many(800, 49152, TcpState::TimeWait, 8081, 7);
        socks.extend(many(200, 50052, TcpState::TimeWait, 8081, 8));
        let top = aggregate(&socks, 49152, 65535, loopback, named)
            .top
            .expect("top");
        assert_eq!(top.client_pid, Some(7));
    }

    #[test]
    fn top_none_below_half_of_in_use() {
        // The established pool is the largest group, but only TIME_WAIT
        // sockets make a storm: 400 of 1000 in use is under half.
        let mut socks = many(600, 49152, TcpState::Other, 443, 7);
        socks.extend(many(400, 50000, TcpState::TimeWait, 3000, 8));
        let p = aggregate(&socks, 49152, 65535, loopback, named);
        assert_eq!(p.in_use, 1000);
        assert_eq!(p.time_wait, 400);
        assert_eq!(p.top, None);
    }

    #[test]
    fn exactly_half_is_top() {
        let mut socks = many(500, 49152, TcpState::Other, 443, 7);
        socks.extend(many(500, 50000, TcpState::TimeWait, 3000, 8));
        let top = aggregate(&socks, 49152, 65535, loopback, named)
            .top
            .expect("top");
        assert_eq!((top.port, top.sockets), (3000, 500));
    }

    #[test]
    fn top_is_the_largest_time_wait_group() {
        let mut socks = many(300, 49152, TcpState::TimeWait, 3000, 7);
        socks.extend(many(700, 50000, TcpState::TimeWait, 8081, 8));
        let top = aggregate(&socks, 49152, 65535, loopback, named)
            .top
            .expect("top");
        assert_eq!(
            (top.port, top.sockets, top.client_pid),
            (8081, 700, Some(8))
        );
    }

    #[test]
    fn listen_and_out_of_range_not_counted() {
        let socks = [
            sock(TcpState::Listen, 50000, 0, 7),
            sock(TcpState::TimeWait, 8081, 50001, 7),
            sock(TcpState::TimeWait, 49151, 3000, 7),
        ];
        let p = aggregate(&socks, 49152, 65535, loopback, named);
        assert_eq!(p.in_use, 0);
        assert_eq!(p.time_wait, 0);
        assert_eq!(p.top, None);
        // The range bounds themselves count.
        let socks = [
            sock(TcpState::TimeWait, 49152, 3000, 7),
            sock(TcpState::TimeWait, 65535, 3000, 7),
        ];
        assert_eq!(aggregate(&socks, 49152, 65535, loopback, named).in_use, 2);
    }

    #[test]
    fn client_name_none_when_pid_gone() {
        let socks = many(10, 49152, TcpState::TimeWait, 8081, 7);
        let top = aggregate(&socks, 49152, 65535, loopback, |_| None)
            .top
            .expect("top");
        assert_eq!(top.client_pid, Some(7));
        assert_eq!(top.client_name, None);
    }

    #[test]
    fn local_false_for_remote_address() {
        let mut socks = many(10, 49152, TcpState::TimeWait, 443, 7);
        socks[3].remote_addr = v4([93, 184, 216, 34]);
        let top = aggregate(&socks, 49152, 65535, loopback, named)
            .top
            .expect("top");
        assert!(!top.local);
        for s in &mut socks {
            s.remote_addr = v4([93, 184, 216, 34]);
        }
        let top = aggregate(&socks, 49152, 65535, loopback, named)
            .top
            .expect("top");
        assert!(!top.local);
    }

    #[test]
    fn empty_or_inverted_range() {
        let p = aggregate(&[], 49152, 65535, loopback, named);
        assert_eq!((p.in_use, p.time_wait, p.top), (0, 0, None));
        let socks = many(10, 49152, TcpState::TimeWait, 443, 7);
        let p = aggregate(&socks, 60000, 50000, loopback, named);
        assert_eq!((p.range_size, p.in_use), (0, 0));
    }
}
