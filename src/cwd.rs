//! Look up the current working directory of a process by PID.
//!
//! Used to give new tabs the same cwd the user has `cd`d to in the active
//! tab. This is the same trick alacritty uses; it works without any shell
//! configuration because the kernel always knows where the process is.
//!
//! Two platforms are supported:
//! - Linux: `readlink /proc/<pid>/cwd` (cheap; std-only).
//! - macOS: `proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, …)` via libc.
//!
//! Everywhere else this returns `None`, and `App::spawn_tab` falls back to
//! the launcher's cwd as before.

use std::path::PathBuf;

/// Resolve the working directory of `pid`. Returns `None` if the process
/// has exited, the lookup is unsupported on this OS, or the OS reported
/// an error (often "permission denied" if `pid` belongs to another user).
pub fn cwd_of_pid(pid: u32) -> Option<PathBuf> {
    cwd_of_pid_impl(pid)
}

/// Best-effort lookup of the foreground process of the tty controlling
/// `shell_pid`. Returns the PID of the process group leader running in
/// the foreground — i.e. the program the user is currently looking at
/// (`htop`, `vim`, etc.). When the shell itself is foreground (sitting at
/// the prompt), this returns `Some(shell_pid)`. Returns `None` when the
/// platform isn't supported, the shell has no controlling terminal, or
/// the lookup failed.
pub fn foreground_pid(shell_pid: u32) -> Option<u32> {
    foreground_pid_impl(shell_pid)
}

#[cfg(target_os = "linux")]
fn foreground_pid_impl(shell_pid: u32) -> Option<u32> {
    // /proc/<pid>/stat field 8 is `tpgid`: the foreground process group
    // of the controlling terminal. The kernel updates it whenever the
    // shell calls tcsetpgrp() to hand the tty to a child job, so reading
    // it gives us the pid of whatever's currently in the foreground.
    //
    // The comm field (field 2) is wrapped in parentheses and may itself
    // contain spaces and parens, so we anchor parsing on the *last* `)`
    // rather than splitting on whitespace from the start.
    let stat = std::fs::read_to_string(format!("/proc/{shell_pid}/stat")).ok()?;
    let rparen = stat.rfind(')')?;
    // After ")" comes " S 563 ..." — fields 3..N separated by spaces.
    // tpgid is field 8, i.e. the 6th field after state.
    let mut fields = stat[rparen + 1..].split_ascii_whitespace();
    let tpgid: i32 = fields.nth(5)?.parse().ok()?;
    if tpgid <= 0 {
        return None;
    }
    Some(tpgid as u32)
}

#[cfg(not(target_os = "linux"))]
fn foreground_pid_impl(_shell_pid: u32) -> Option<u32> {
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
    #[cfg(target_os = "linux")]
    fn foreground_pid_parses_proc_stat() {
        // Self-lookup: we have no controlling tty in the test runner, so
        // tpgid is -1 and this returns None. Just exercise the parser
        // path and make sure it doesn't panic on a real stat file.
        let _ = foreground_pid(std::process::id());
    }

    #[test]
    fn foreground_pid_nonexistent_returns_none() {
        assert!(foreground_pid(0).is_none());
    }
}
