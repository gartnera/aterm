//! Look up process working directories and the tty's foreground process.
//!
//! Two uses:
//! - Seed a new tab's cwd from the active tab (the same trick alacritty
//!   uses; works without shell config because the kernel always knows
//!   where a process is).
//! - Show the foreground program's name + cwd in the tab title.
//!
//! Platform support:
//! - Linux: `/proc/<pid>/cwd` and `/proc/<pid>/comm` (std-only).
//! - macOS: `proc_pidinfo`/`proc_name` via libc.
//! - Foreground process lookup uses `tcgetpgrp()` on the pty master fd on
//!   every unix.
//!
//! Everywhere unsupported these return `None`, and callers fall back to
//! the launcher's cwd / the bare title as before.

use std::path::PathBuf;

#[cfg(unix)]
use std::os::fd::RawFd;

/// Resolve the working directory of `pid`. Returns `None` if the process
/// has exited, the lookup is unsupported on this OS, or the OS reported
/// an error (often "permission denied" if `pid` belongs to another user).
pub fn cwd_of_pid(pid: u32) -> Option<PathBuf> {
    cwd_of_pid_impl(pid)
}

/// Best-effort lookup of the foreground process of the terminal referred
/// to by `tty_fd` (a fd open on the pty master). Returns the PID of the
/// process group leader running in the foreground — i.e. the program the
/// user is currently looking at (`htop`, `vim`, etc.). When the shell
/// itself is foreground (sitting at the prompt) this returns the shell's
/// pgid, which equals the shell pid for an interactive shell. Returns
/// `None` when unsupported, the terminal has no foreground group, or the
/// call failed.
#[cfg(unix)]
pub fn foreground_pid(tty_fd: RawFd) -> Option<u32> {
    // tcgetpgrp() reports the foreground process group of the controlling
    // terminal. The kernel updates it whenever the shell calls tcsetpgrp()
    // to hand the tty to a child job, so it tracks the foreground program
    // without any shell config. The group leader's pid equals the pgid, so
    // we can pass the result straight to cwd_of_pid / process_name.
    let pgrp = unsafe { libc::tcgetpgrp(tty_fd) };
    if pgrp <= 0 {
        return None;
    }
    Some(pgrp as u32)
}

#[cfg(not(unix))]
pub fn foreground_pid(_tty_fd: i32) -> Option<u32> {
    None
}

/// Human-readable name of the process `pid` is running — e.g. `htop`,
/// `vim`, `node`. Returns `None` on unsupported platforms or if the
/// process has exited.
pub fn process_name(pid: u32) -> Option<String> {
    process_name_impl(pid)
}

/// Name of the command as the user invoked it — the basename of the
/// process's `argv[0]`. Unlike [`process_name`] (the kernel's record of the
/// executed file) this follows the symlink the user actually typed: a tool
/// installed as `…/versions/2.1.150` behind a `claude` symlink reports
/// `claude` here but `2.1.150` from [`process_name`]. Returns `None` when
/// unsupported, the args are unreadable (the process exited, or on macOS is
/// owned by another user), or `argv[0]` is empty.
pub fn invoked_name(pid: u32) -> Option<String> {
    invoked_name_impl(pid)
}

#[cfg(target_os = "linux")]
fn process_name_impl(pid: u32) -> Option<String> {
    // /proc/<pid>/comm is the command name (truncated by the kernel to 15
    // chars), newline-terminated. It tracks exec() and prctl(PR_SET_NAME),
    // so it reflects whatever the process currently calls itself.
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = comm.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

#[cfg(target_os = "macos")]
fn process_name_impl(pid: u32) -> Option<String> {
    // proc_name() copies the process's (accounting) name into the buffer
    // and returns the byte count. The name is at most 2*MAXCOMLEN; 256 is
    // comfortably large. SAFETY: we pass a buffer and its true length, and
    // only read the bytes the call reports it wrote.
    let mut buf = [0u8; 256];
    let written = unsafe {
        libc::proc_name(
            pid as libc::c_int,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if written <= 0 {
        return None;
    }
    let written = (written as usize).min(buf.len());
    // proc_name may or may not include a trailing NUL in the count; stop
    // at the first NUL to be safe.
    let end = buf[..written]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(written);
    let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_name_impl(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn invoked_name_impl(pid: u32) -> Option<String> {
    // /proc/<pid>/cmdline is the argv vector, NUL-separated; the first field
    // is argv[0] as passed to execve — the PATH-resolved command word
    // ("claude"), not the symlink target the kernel records in comm
    // ("2.1.150"). A process can overwrite this via setproctitle; we take its
    // own claim, same as we trust comm.
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv0 = cmdline.split(|&b| b == 0).next()?;
    basename_of(argv0)
}

#[cfg(target_os = "macos")]
fn invoked_name_impl(pid: u32) -> Option<String> {
    use std::cell::RefCell;
    // KERN_PROCARGS2 hands back, for `pid`, a blob laid out as:
    //   [i32 argc][exec_path\0][\0 padding…][argv[0]\0][argv[1]\0]…[env…]
    // We want argv[0] — the name as invoked — which, unlike the exec_path or
    // proc_name, preserves the symlink the user typed. This is how `ps`
    // recovers the command name. sysctl returns an error for a process owned
    // by another user, so setuid tools fall through to process_name.
    //
    // The blob can be up to kern.argmax (~1 MiB); reuse one buffer per thread
    // so a busy tab repaint doesn't churn the allocator.
    thread_local! {
        static BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }
    let argmax = kern_argmax()?;
    BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        buf.resize(argmax, 0);
        let mut len = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        // SAFETY: `mib` is a valid 3-element MIB; we pass the buffer with its
        // true capacity in `len`, and read only the `len` bytes sysctl reports
        // it wrote back.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len < std::mem::size_of::<libc::c_int>() {
            return None;
        }
        let blob = &buf[..len];
        // Skip argc, then the exec_path string, then the NUL padding before
        // argv[0]; read argv[0] up to its terminating NUL.
        let mut pos = std::mem::size_of::<libc::c_int>();
        while pos < blob.len() && blob[pos] != 0 {
            pos += 1;
        }
        while pos < blob.len() && blob[pos] == 0 {
            pos += 1;
        }
        let start = pos;
        while pos < blob.len() && blob[pos] != 0 {
            pos += 1;
        }
        basename_of(&blob[start..pos])
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn invoked_name_impl(_pid: u32) -> Option<String> {
    None
}

/// Read kern.argmax (the per-boot upper bound on a process's args+env size)
/// once. KERN_PROCARGS2 wants a buffer this large to guarantee it can return
/// the whole blob.
#[cfg(target_os = "macos")]
fn kern_argmax() -> Option<usize> {
    use std::sync::OnceLock;
    static ARGMAX: OnceLock<Option<usize>> = OnceLock::new();
    *ARGMAX.get_or_init(|| {
        let mut argmax: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: 2-element MIB; `argmax`/`len` are out-params of the matching
        // type and size.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                &mut argmax as *mut libc::c_int as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && argmax > 0).then_some(argmax as usize)
    })
}

/// Basename of a raw `argv[0]`, stripping the leading `-` a login shell adds
/// (`-zsh` → `zsh`). Returns `None` for an empty arg.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn basename_of(arg: &[u8]) -> Option<String> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let base = std::path::Path::new(OsStr::from_bytes(arg)).file_name()?;
    let base = base.to_string_lossy();
    let base = base.strip_prefix('-').unwrap_or(&base);
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

#[cfg(target_os = "linux")]
fn cwd_of_pid_impl(pid: u32) -> Option<PathBuf> {
    // /proc/<pid>/cwd is a symlink to the process's current working
    // directory. read_link returns the symlink target without resolving
    // mount-namespace differences — exactly what we want.
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
fn cwd_of_pid_impl(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: proc_pidinfo writes at most `buffersize` bytes into the
    // provided buffer. We pass a properly sized `MaybeUninit<…>` and only
    // read the field we care about (pvi_cdir.vip_path) after the call
    // succeeds with a positive return value.
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if written <= 0 {
        return None;
    }
    // The kernel returns the number of bytes written; require at least
    // the offset+len of pvi_cdir.vip_path so we don't read past the
    // populated region.
    if (written as usize) < std::mem::size_of::<libc::proc_vnodepathinfo>() {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let raw = info.pvi_cdir.vip_path;
    // vip_path is a fixed-size NUL-terminated UTF-8 path. CStr::from_ptr
    // stops at the first NUL.
    let cstr = unsafe { CStr::from_ptr(raw.as_ptr().cast::<libc::c_char>()) };
    let bytes = cstr.to_bytes();
    if bytes.is_empty() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cwd_of_pid_impl(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn current_process_cwd_matches_std() {
        // self-lookup: should agree with std::env::current_dir() on the
        // supported platforms.
        let want = std::env::current_dir().unwrap();
        let got = cwd_of_pid(std::process::id()).expect("self cwd lookup");
        // Compare after canonicalize to absorb /private prefixes on macOS
        // and any symlink hops.
        let want = std::fs::canonicalize(&want).unwrap_or(want);
        let got = std::fs::canonicalize(&got).unwrap_or(got);
        assert_eq!(got, want);
    }

    #[test]
    fn nonexistent_pid_returns_none() {
        // PID 0 is never a real process. The kernel returns EINVAL/ESRCH;
        // we map either to None.
        assert!(cwd_of_pid(0).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn foreground_pid_bad_fd_returns_none() {
        // -1 is never a valid fd; tcgetpgrp sets EBADF and we map to None.
        assert!(foreground_pid(-1).is_none());
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn process_name_self_is_nonempty() {
        let name = process_name(std::process::id()).expect("self process name");
        assert!(!name.is_empty());
    }

    #[test]
    fn process_name_nonexistent_returns_none() {
        assert!(process_name(0).is_none());
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn invoked_name_self_is_nonempty() {
        // argv[0] of the test binary is some path; we only assert we can read
        // and basename it (the symlink-following value is exercised e2e).
        let name = invoked_name(std::process::id()).expect("self invoked name");
        assert!(!name.is_empty());
    }

    #[test]
    fn invoked_name_nonexistent_returns_none() {
        // pid 0 has no readable argv on either platform.
        assert!(invoked_name(0).is_none());
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn basename_of_strips_path_and_login_dash() {
        assert_eq!(basename_of(b"claude").as_deref(), Some("claude"));
        assert_eq!(
            basename_of(b"/Users/x/.local/share/claude/versions/2.1.150").as_deref(),
            Some("2.1.150")
        );
        assert_eq!(basename_of(b"./ssh").as_deref(), Some("ssh"));
        assert_eq!(basename_of(b"-zsh").as_deref(), Some("zsh"));
        assert_eq!(basename_of(b""), None);
    }
}
