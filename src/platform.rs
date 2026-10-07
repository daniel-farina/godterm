//! The few Unix only calls GodTerm makes, behind one cross platform face so
//! the rest of the code reads the same everywhere. On Unix these are the
//! real calls. On Windows they do the nearest equivalent, or nothing where
//! the concept does not exist (Unix permission bits: files under the user's
//! profile are already private to the user through their ACLs).

use std::fs::File;
use std::path::Path;

#[cfg(unix)]
pub use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

#[cfg(windows)]
pub use win::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

#[cfg(unix)]
pub use libc::{SIGHUP, SIGINT, SIGKILL, SIGTERM};
#[cfg(windows)]
pub const SIGHUP: i32 = 1;
#[cfg(windows)]
#[allow(dead_code)] // Ctrl-C goes through procs.rs on Windows
pub const SIGINT: i32 = 2;
#[cfg(windows)]
pub const SIGKILL: i32 = 9;
#[cfg(windows)]
pub const SIGTERM: i32 = 15;

/// The signals that end the main loop cleanly (window closed, Ctrl-C, kill).
#[cfg(unix)]
pub const EXIT_SIGNALS: &[i32] = &[
    signal_hook::consts::SIGTERM,
    signal_hook::consts::SIGHUP,
    signal_hook::consts::SIGINT,
    signal_hook::consts::SIGQUIT,
];
#[cfg(windows)]
pub const EXIT_SIGNALS: &[i32] = &[signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT];

/// kill(2): `sig` 0 only checks that `pid` exists. 0 on success, like libc.
/// On Windows any other signal terminates the process.
pub fn kill(pid: i32, sig: i32) -> i32 {
    #[cfg(unix)]
    {
        // SAFETY: plain kill(2); callers pass pids they started or were asked to stop.
        unsafe { libc::kill(pid, sig) }
    }
    #[cfg(windows)]
    {
        win::kill(pid as u32, sig)
    }
}

/// Fill `buf` from the OS random source (/dev/urandom; the system RNG on
/// Windows). Session ids and the control socket tokens come from here.
pub fn fill_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = File::open("/dev/urandom") {
            let _ = f.read_exact(buf);
        }
    }
    #[cfg(windows)]
    {
        let _ = getrandom::fill(buf);
    }
}

/// Whether a process with this pid is running.
pub fn alive(pid: u32) -> bool {
    kill(pid as i32, 0) == 0
}

/// Take an exclusive lock without waiting (flock LOCK_EX | LOCK_NB).
pub fn try_lock(f: &File) -> bool {
    f.try_lock().is_ok()
}

/// Take an exclusive lock, waiting for it (flock LOCK_EX).
pub fn lock(f: &File) -> bool {
    f.lock().is_ok()
}

/// Release a lock taken with `try_lock` or `lock`.
pub fn unlock(f: &File) {
    let _ = f.unlock();
}

/// Whether a folder can be created in `dir` (it exists and is writable).
pub fn writable(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: access(2) on a NUL terminated path.
        dir.is_dir() && unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0
    }
    #[cfg(windows)]
    {
        std::fs::metadata(dir).is_ok_and(|m| m.is_dir() && !m.permissions().readonly())
    }
}

/// A symbolic link at `link` pointing at `target`.
pub fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Become `bin` with `args` (the same pid, the same terminal): exec(3) on
/// Unix. Windows has no exec, so the new program runs as a child sharing
/// this console and this process exits with its code. Returns only on
/// failure.
pub fn exec_replace(bin: &Path, args: &[String]) -> std::io::Error {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.exec()
    }
    #[cfg(windows)]
    {
        match cmd.status() {
            Ok(st) => std::process::exit(st.code().unwrap_or(0)),
            Err(e) => e,
        }
    }
}

/// The terminal device on stdin, e.g. /dev/ttys012 (None on Windows).
pub fn own_tty() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: ttyname returns a pointer to static storage or null.
        let p = unsafe { libc::ttyname(0) };
        if p.is_null() {
            return None;
        }
        // SAFETY: non null, NUL terminated.
        Some(
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned(),
        )
    }
    #[cfg(windows)]
    {
        None
    }
}

/// A short directory for sockets when the home's path is too long.
pub fn short_socket_dir() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        std::path::PathBuf::from("/tmp")
    }
    #[cfg(windows)]
    {
        std::env::temp_dir()
    }
}

/// The current user's numeric id (for per user names); 0 on Windows.
pub fn uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no failure modes.
        unsafe { libc::getuid() }
    }
    #[cfg(windows)]
    {
        0
    }
}

/// Run `f` with the process umask set to `mask` (Unix), restoring it after.
pub fn with_umask<T>(mask: u32, f: impl FnOnce() -> T) -> T {
    #[cfg(unix)]
    {
        // SAFETY: umask is process wide; set and restored around `f`.
        let old = unsafe { libc::umask(mask as libc::mode_t) };
        let r = f();
        // SAFETY: restoring the previous mask.
        unsafe { libc::umask(old) };
        r
    }
    #[cfg(windows)]
    {
        let _ = mask;
        f()
    }
}

#[cfg(windows)]
mod win {
    use std::fs::{DirBuilder, OpenOptions, Permissions};

    /// Unix permission bits on Windows: `mode` is accepted and ignored.
    pub trait OpenOptionsExt {
        fn mode(&mut self, mode: u32) -> &mut Self;
    }
    impl OpenOptionsExt for OpenOptions {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }

    pub trait DirBuilderExt {
        fn mode(&mut self, mode: u32) -> &mut Self;
    }
    impl DirBuilderExt for DirBuilder {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }

    /// Only the write bits mean something: they map to the read only flag.
    pub trait PermissionsExt {
        fn from_mode(mode: u32) -> Self;
        fn mode(&self) -> u32;
    }
    impl PermissionsExt for Permissions {
        fn from_mode(mode: u32) -> Self {
            // Permissions has no public constructor on Windows: start from
            // a writable file's and set the read only flag from the mode.
            let mut p = std::env::current_exe()
                .and_then(std::fs::metadata)
                .or_else(|_| std::fs::metadata(std::env::temp_dir()))
                .map(|m| m.permissions())
                .expect("no file to take permissions from");
            p.set_readonly(mode & 0o222 == 0);
            p
        }
        fn mode(&self) -> u32 {
            if self.readonly() {
                0o444
            } else {
                0o644
            }
        }
    }

    pub fn kill(pid: u32, sig: i32) -> i32 {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_TERMINATE,
        };
        const STILL_ACTIVE: u32 = 259;
        let access = if sig == 0 {
            PROCESS_QUERY_LIMITED_INFORMATION
        } else {
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION
        };
        // SAFETY: OpenProcess returns null on failure; the handle is closed below.
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() {
            return -1;
        }
        let ok = if sig == 0 {
            let mut code = 0u32;
            // SAFETY: valid handle and out pointer.
            (unsafe { GetExitCodeProcess(h, &mut code) } != 0) && code == STILL_ACTIVE
        } else {
            // SAFETY: valid handle opened with PROCESS_TERMINATE.
            unsafe { TerminateProcess(h, 1) != 0 }
        };
        // SAFETY: closing the handle we opened.
        unsafe { CloseHandle(h) };
        if ok {
            0
        } else {
            -1
        }
    }
}
