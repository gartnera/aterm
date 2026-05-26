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
}
