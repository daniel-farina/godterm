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
#[allow(dead_code)]
pub const SIGINT: i32 = 2;
#[cfg(windows)]
pub const SIGKILL: i32 = 9;
#[cfg(windows)]
pub const SIGTERM: i32 = 15;

#[cfg(unix)]
pub const EXIT_SIGNALS: &[i32] = &[
    signal_hook::consts::SIGTERM,
    signal_hook::consts::SIGHUP,
    signal_hook::consts::SIGINT,
    signal_hook::consts::SIGQUIT,
];
#[cfg(windows)]
pub const EXIT_SIGNALS: &[i32] = &[signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT];

pub fn kill(pid: i32, sig: i32) -> i32 {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid, sig) }
    }
    #[cfg(windows)]
    {
        win::kill(pid as u32, sig)
    }
}

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

pub fn alive(pid: u32) -> bool {
    kill(pid as i32, 0) == 0
}

pub fn try_lock(f: &File) -> bool {
    f.try_lock().is_ok()
}

pub fn lock(f: &File) -> bool {
    f.lock().is_ok()
}

pub fn unlock(f: &File) {
    let _ = f.unlock();
}

pub fn writable(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };

        dir.is_dir() && unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0
    }
    #[cfg(windows)]
    {
        std::fs::metadata(dir).is_ok_and(|m| m.is_dir() && !m.permissions().readonly())
    }
}

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

pub fn own_tty() -> Option<String> {
    #[cfg(unix)]
    {
        let p = unsafe { libc::ttyname(0) };
        if p.is_null() {
            return None;
        }

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

pub fn short_socket_dir() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::path::PathBuf;
        let me = uid();
        let private = |d: &std::path::Path| {
            std::fs::symlink_metadata(d)
                .is_ok_and(|m| m.is_dir() && m.uid() == me && m.mode() & 0o022 == 0)
        };
        let env = if cfg!(target_os = "macos") {
            "TMPDIR"
        } else {
            "XDG_RUNTIME_DIR"
        };
        if let Some(d) = std::env::var_os(env).map(PathBuf::from) {
            let d = d.canonicalize().unwrap_or(d);
            if d.is_absolute() && d.as_os_str().len() < 64 && private(&d) {
                return d;
            }
        }
        let d = PathBuf::from(format!("/tmp/godterm-{me}"));
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&d);
        if private(&d) {
            return d;
        }
        PathBuf::from("/tmp")
    }
    #[cfg(windows)]
    {
        std::env::temp_dir()
    }
}

pub fn uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(windows)]
    {
        0
    }
}

pub fn with_umask<T>(mask: u32, f: impl FnOnce() -> T) -> T {
    #[cfg(unix)]
    {
        let old = unsafe { libc::umask(mask as libc::mode_t) };
        let r = f();

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

    pub trait PermissionsExt {
        fn from_mode(mode: u32) -> Self;
        fn mode(&self) -> u32;
    }
    impl PermissionsExt for Permissions {
        fn from_mode(mode: u32) -> Self {
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

        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() {
            return -1;
        }
        let ok = if sig == 0 {
            let mut code = 0u32;

            (unsafe { GetExitCodeProcess(h, &mut code) } != 0) && code == STILL_ACTIVE
        } else {
            unsafe { TerminateProcess(h, 1) != 0 }
        };

        unsafe { CloseHandle(h) };
        if ok {
            0
        } else {
            -1
        }
    }
}
