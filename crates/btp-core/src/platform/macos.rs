//! macOS implementation, entirely in-process: libproc for pids, process info
//! and socket state, `sysctl(KERN_PROCARGS2)` for the command line, and
//! `kill(2)` for signals. No subprocess, no lsof.
//!
//! Processes we may not inspect (other users, permission denied, gone
//! between two calls) are skipped silently.

use super::{format_proc_start, Listener, Platform, ProcInfo};
use libproc::bsd_info::BSDInfo;
use libproc::file_info::{pidfdinfo, ListFDs, ProcFDType};
use libproc::net_info::{SocketFDInfo, SocketInfoKind, TcpSIState, TcpSockInfo};
use libproc::proc_pid::{listpidinfo, pidinfo};
use libproc::processes::{pids_by_type, ProcFilter};
use std::collections::HashSet;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

/// macOS platform, backed by libproc.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacOs;

fn bsd_info(pid: i32) -> Option<BSDInfo> {
    if pid <= 0 {
        return None;
    }
    pidinfo::<BSDInfo>(pid, 0).ok()
}

fn start_time_us(info: &BSDInfo) -> u64 {
    info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec
}

/// Calls `f` with every TCP socket (IPv4 or IPv6) `pid` holds.
fn for_each_tcp_socket(pid: i32, mut f: impl FnMut(&TcpSockInfo)) {
    // SAFETY: a null buffer with size 0 is the documented way to ask
    // proc_pidinfo(PROC_PIDLISTFDS) for the buffer size it needs; nothing is
    // written.
    let bytes =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return;
    }
    let max = bytes as usize / std::mem::size_of::<libproc::file_info::ProcFDInfo>();
    let Ok(fds) = listpidinfo::<ListFDs>(pid, max) else {
        return;
    };
    for fd in fds {
        if !matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::Socket) {
            continue;
        }
        let Ok(sock) = pidfdinfo::<SocketFDInfo>(pid, fd.proc_fd) else {
            continue;
        };
        let family = sock.psi.soi_family;
        if !matches!(SocketInfoKind::from(sock.psi.soi_kind), SocketInfoKind::Tcp)
            || (family != libc::AF_INET && family != libc::AF_INET6)
        {
            continue;
        }
        // SAFETY: soi_kind == SOCKINFO_TCP means the kernel filled the
        // `pri_tcp` member of the `soi_proto` union.
        let tcp = unsafe { sock.psi.soi_proto.pri_tcp };
        f(&tcp);
    }
}

/// Local port of a TCP socket. `insi_lport` holds the port in network byte
/// order in its low 16 bits.
fn local_port(tcp: &TcpSockInfo) -> u16 {
    u16::from_be(tcp.tcpsi_ini.insi_lport as u16)
}

fn tcp_state(tcp: &TcpSockInfo) -> TcpSIState {
    TcpSIState::from(tcp.tcpsi_state)
}

fn cwd(pid: i32) -> Option<PathBuf> {
    // SAFETY: proc_vnodepathinfo is plain old data (integers and char
    // arrays), so all-zero is a valid value.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as c_int;
    // SAFETY: the buffer is a proc_vnodepathinfo of exactly `size` bytes, as
    // PROC_PIDVNODEPATHINFO expects; the kernel writes at most `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast::<c_void>(),
            size,
        )
    };
    if n != size {
        return None;
    }
    // vip_path is char[MAXPATHLEN], declared by libc as [[c_char; 32]; 32].
    let raw = &info.pvi_cdir.vip_path;
    // SAFETY: [[c_char; 32]; 32] is 1024 contiguous c_char.
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), std::mem::size_of_val(raw))
    };
    let path = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn arg_max() -> usize {
    static ARG_MAX: OnceLock<usize> = OnceLock::new();
    *ARG_MAX.get_or_init(|| {
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        let mut value: c_int = 0;
        let mut len = std::mem::size_of::<c_int>();
        // SAFETY: kern.argmax is an int; `value` and `len` describe a buffer
        // of exactly that size.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                (&mut value as *mut c_int).cast::<c_void>(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && value > 0 {
            value as usize
        } else {
            1 << 20
        }
    })
}

/// argv joined by spaces, from `sysctl(KERN_PROCARGS2)`. Layout: `int argc`,
/// the exec path, its NUL terminator padded with NULs so argv starts on an
/// 8-byte boundary of the string area (which begins right after argc), then
/// `argc` NUL-terminated arguments, then the environment.
fn command_line(pid: i32) -> Option<String> {
    parse_procargs2(&procargs2(pid)?)
}

/// Raw `sysctl(KERN_PROCARGS2)` buffer, `None` when unreadable.
fn procargs2(pid: i32) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; arg_max()];
    let mut len = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: `buf` is `len` writable bytes; the kernel writes at most `len`
    // bytes and updates `len` to the count written.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    Some(buf)
}

/// Walks the KERN_PROCARGS2 layout: the `argc` arguments, and the bytes
/// after them (the environment area).
fn split_procargs2(data: &[u8]) -> Option<(Vec<&[u8]>, &[u8])> {
    const ALIGN: usize = 8;
    let argc = i32::from_ne_bytes(data.get(..4)?.try_into().ok()?);
    if argc <= 0 {
        return None;
    }
    let strings = &data[4..];
    let exec_end = strings.iter().position(|&b| b == 0)?;
    // Skip exactly the terminator and the alignment padding, never "every
    // NUL": an empty argv[0] is one more NUL, and swallowing it would shift
    // argv by one and pull an environment string (maybe a secret) into the
    // command. Padding that is not all NULs means a layout we do not know:
    // give up and let the caller fall back to the process name.
    let args_start = (exec_end + 1).div_ceil(ALIGN) * ALIGN;
    let padding = strings.get(exec_end..args_start)?;
    if padding.iter().any(|&b| b != 0) {
        return None;
    }
    let mut rest = &strings[args_start..];
    let mut args = Vec::new();
    while args.len() < argc as usize {
        let Some(end) = rest.iter().position(|&b| b == 0) else {
            // Last argument without a terminator: nothing follows it.
            args.push(rest);
            rest = &[];
            break;
        };
        args.push(&rest[..end]);
        rest = &rest[end + 1..];
    }
    Some((args, rest))
}

fn parse_procargs2(data: &[u8]) -> Option<String> {
    let (args, _) = split_procargs2(data)?;
    let args: Vec<String> = args
        .into_iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    (!args.is_empty()).then(|| args.join(" "))
}

/// The `NAME=value` strings after the arguments. Entries without `=` are
/// skipped; the list ends at the first empty entry (the kernel's padding).
fn parse_procargs2_env(data: &[u8]) -> Option<Vec<(String, String)>> {
    let (args, env) = split_procargs2(data)?;
    if args.is_empty() {
        return None;
    }
    Some(
        env.split(|&b| b == 0)
            .take_while(|e| !e.is_empty())
            .filter_map(|e| {
                let eq = e.iter().position(|&b| b == b'=')?;
                Some((
                    String::from_utf8_lossy(&e[..eq]).into_owned(),
                    String::from_utf8_lossy(&e[eq + 1..]).into_owned(),
                ))
            })
            .collect(),
    )
}

/// `PROC_PIDFDVNODEPATHINFO` (sys/proc_info.h), not exported by libc.
const PROC_PIDFDVNODEPATHINFO: c_int = 2;

/// `struct vnode_fdinfowithpath`: `proc_fileinfo` (24 bytes) followed by the
/// vnode info and path.
#[repr(C)]
struct VnodeFdInfoWithPath {
    _pfi: [u8; 24],
    pvip: libc::vnode_info_path,
}

/// Paths of the vnode (file or directory) descriptors `pid` holds.
fn vnode_paths(pid: i32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if pid <= 0 {
        return out;
    }
    // SAFETY: a null buffer with size 0 only asks for the size needed.
    let bytes =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return out;
    }
    let max = bytes as usize / std::mem::size_of::<libproc::file_info::ProcFDInfo>();
    let Ok(fds) = listpidinfo::<ListFDs>(pid, max) else {
        return out;
    };
    let size = std::mem::size_of::<VnodeFdInfoWithPath>() as c_int;
    for fd in fds {
        if !matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::VNode) {
            continue;
        }
        // SAFETY: plain old data (integers and char arrays); all-zero is valid.
        let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
        // SAFETY: the buffer is `size` writable bytes laid out like the
        // kernel's vnode_fdinfowithpath; the kernel writes at most `size`.
        let n = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                (&mut info as *mut VnodeFdInfoWithPath).cast::<c_void>(),
                size,
            )
        };
        // The descriptor may have been closed since the list: skip it.
        if n != size {
            continue;
        }
        let raw = &info.pvip.vip_path;
        // SAFETY: [[c_char; 32]; 32] is 1024 contiguous c_char.
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), std::mem::size_of_val(raw))
        };
        if let Ok(path) = CStr::from_bytes_until_nul(bytes) {
            if let Ok(path) = path.to_str() {
                if !path.is_empty() {
                    out.push(PathBuf::from(path));
                }
            }
        }
    }
    out
}

fn c_name(raw: &[c_char]) -> String {
    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl Platform for MacOs {
    fn listeners(&self) -> Vec<Listener> {
        // Only our own processes can be inspected unless we are root, so
        // listing by uid saves one failing syscall per foreign process.
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        let filter = if uid == 0 {
            ProcFilter::All
        } else {
            ProcFilter::ByUID { uid }
        };
        let Ok(pids) = pids_by_type(filter) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for pid in pids {
            let Ok(pid) = i32::try_from(pid) else {
                continue;
            };
            if pid <= 0 {
                continue;
            }
            for_each_tcp_socket(pid, |tcp| {
                if matches!(tcp_state(tcp), TcpSIState::Listen) {
                    let l = Listener {
                        pid,
                        port: local_port(tcp),
                    };
                    if seen.insert(l) {
                        out.push(l);
                    }
                }
            });
        }
        out.sort_by_key(|l| (l.port, l.pid));
        out
    }

    fn proc_info(&self, pid: i32) -> Option<ProcInfo> {
        let info = bsd_info(pid)?;
        let command = command_line(pid).unwrap_or_else(|| {
            let name = c_name(&info.pbi_name);
            if name.is_empty() {
                c_name(&info.pbi_comm)
            } else {
                name
            }
        });
        Some(ProcInfo {
            pid,
            ppid: info.pbi_ppid as i32,
            start_time: start_time_us(&info),
            cwd: cwd(pid),
            command,
        })
    }

    fn established(&self, pid: i32, port: u16) -> u32 {
        if pid <= 0 {
            return 0;
        }
        let mut n = 0;
        for_each_tcp_socket(pid, |tcp| {
            if matches!(tcp_state(tcp), TcpSIState::Established) && local_port(tcp) == port {
                n += 1;
            }
        });
        n
    }

    fn descendants(&self, pid: i32) -> Vec<i32> {
        let mut out = Vec::new();
        if pid <= 0 {
            return out;
        }
        let mut seen = HashSet::from([pid]);
        let mut queue = std::collections::VecDeque::from([pid]);
        while let Some(parent) = queue.pop_front() {
            let Ok(children) = pids_by_type(ProcFilter::ByParentProcess {
                ppid: parent as u32,
            }) else {
                continue;
            };
            for child in children {
                let Ok(child) = i32::try_from(child) else {
                    continue;
                };
                if child > 0 && seen.insert(child) {
                    out.push(child);
                    queue.push_back(child);
                }
            }
        }
        out
    }

    fn signal(&self, pid: i32, sig: i32) -> bool {
        // kill(0, ..) targets our process group and kill(-n, ..) a whole
        // group or every process: never.
        if pid <= 0 {
            return false;
        }
        // SAFETY: kill(2) with a positive pid only affects that process.
        unsafe { libc::kill(pid, sig) == 0 }
    }

    fn proc_start_string(&self, pid: i32) -> Option<String> {
        bsd_info(pid).map(|i| format_proc_start(start_time_us(&i)))
    }

    fn proc_env(&self, pid: i32) -> Option<Vec<(String, String)>> {
        let info = bsd_info(pid)?;
        // Other users' environments are none of our business.
        // SAFETY: geteuid has no preconditions and cannot fail.
        if info.pbi_uid != unsafe { libc::geteuid() } {
            return None;
        }
        parse_procargs2_env(&procargs2(pid)?)
    }

    fn open_paths(&self, pid: i32) -> Vec<PathBuf> {
        vnode_paths(pid)
    }

    fn pids_named(&self, name: &str) -> Vec<i32> {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        let Ok(pids) = pids_by_type(ProcFilter::ByUID { uid }) else {
            return Vec::new();
        };
        let mut out: Vec<i32> = pids
            .into_iter()
            .filter_map(|p| i32::try_from(p).ok())
            .filter(|&p| {
                bsd_info(p)
                    .is_some_and(|i| c_name(&i.pbi_comm) == name || c_name(&i.pbi_name) == name)
            })
            .collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A KERN_PROCARGS2 buffer laid out like the kernel does: argc, exec
    /// path, NUL terminator padded so the arguments start on an 8-byte
    /// boundary of the string area, the arguments, then the environment.
    fn procargs(argc: i32, exec: &str, args: &[&str]) -> Vec<u8> {
        let mut v = argc.to_ne_bytes().to_vec();
        v.extend_from_slice(exec.as_bytes());
        let pad = (8 - (exec.len() + 1) % 8) % 8;
        v.extend(std::iter::repeat_n(0, pad + 1));
        for a in args {
            v.extend_from_slice(a.as_bytes());
            v.push(0);
        }
        v.extend_from_slice(b"PATH=/bin\0SECRET_TOKEN=hunter2\0");
        v
    }

    #[test]
    fn parses_procargs2_args_not_env() {
        let data = procargs(3, "/usr/bin/node", &["node", "vite", "--port=5173"]);
        assert_eq!(
            parse_procargs2(&data).as_deref(),
            Some("node vite --port=5173")
        );
        // Exec path already aligned: no padding at all.
        let data = procargs(1, "/bin/sh", &["sh"]);
        assert_eq!(parse_procargs2(&data).as_deref(), Some("sh"));
    }

    #[test]
    fn empty_argv0_does_not_pull_in_the_environment() {
        let data = procargs(2, "/bin/sleep", &["", "30"]);
        assert_eq!(parse_procargs2(&data).as_deref(), Some(" 30"));
        let data = procargs(3, "/bin/sh", &["", "", "x"]);
        assert_eq!(parse_procargs2(&data).as_deref(), Some("  x"));
    }

    #[test]
    fn matches_a_real_kernel_buffer() {
        // Captured from `exec -a "" /bin/sleep 30` (argv0 empty): 5 padding
        // NULs after the terminator, then argv0's own terminator.
        let mut data = 2i32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/bin/sleep\0\0\0\0\0\0\x0030\0HOME=/x\0");
        assert_eq!(parse_procargs2(&data).as_deref(), Some(" 30"));
    }

    #[test]
    fn unexpected_padding_is_rejected() {
        // Fewer NULs than the alignment requires: the layout is not the one
        // we know, so give up rather than guess where argv starts.
        let mut data = 2i32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/bin/sleep\0\x0030\0SECRET=1\0");
        assert_eq!(parse_procargs2(&data), None);
    }

    #[test]
    fn parses_procargs2_env_after_args() {
        let mut data = 2i32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/bin/sh\0\0sh\0-c\0");
        data.extend_from_slice(b"A=1\0CODEX_THREAD_ID=abc\0noequals\0\0\0");
        assert_eq!(
            parse_procargs2_env(&data),
            Some(vec![
                ("A".to_string(), "1".to_string()),
                ("CODEX_THREAD_ID".to_string(), "abc".to_string())
            ])
        );
        // The shared helper layout: its fixed environment is parsed too.
        let data = procargs(1, "/bin/sh", &["sh"]);
        let env = parse_procargs2_env(&data).unwrap();
        assert_eq!(env[0], ("PATH".to_string(), "/bin".to_string()));
    }

    #[test]
    fn env_parse_rejects_bad_padding() {
        let mut data = 2i32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/bin/sleep\0\x0030\0SECRET=1\0");
        assert_eq!(parse_procargs2_env(&data), None);
    }

    #[test]
    fn rejects_empty_or_short_procargs2() {
        assert_eq!(parse_procargs2(&[]), None);
        assert_eq!(parse_procargs2(&procargs(0, "/bin/x", &[])), None);
        assert_eq!(parse_procargs2(&1i32.to_ne_bytes()), None);
    }
}
