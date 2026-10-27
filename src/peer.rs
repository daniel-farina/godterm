#[cfg(target_os = "macos")]
pub fn pid_of(conn: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;

    let r = unsafe {
        libc::getsockopt(
            conn.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    };
    (r == 0 && pid > 0).then_some(pid as u32)
}

#[cfg(target_os = "linux")]
pub fn pid_of(conn: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let r = unsafe {
        libc::getsockopt(
            conn.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (r == 0 && cred.pid > 0).then_some(cred.pid as u32)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub fn pid_of(_conn: &std::os::unix::net::UnixStream) -> Option<u32> {
    None
}

#[cfg(windows)]
pub fn pid_of(_conn: &uds_windows::UnixStream) -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
pub fn parent_of(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (n == size).then_some(info.pbi_ppid)
}

#[cfg(target_os = "linux")]
pub fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn parent_of(pid: u32) -> Option<u32> {
    #[cfg(windows)]
    {
        return crate::procs::process(pid)
            .map(|p| p.ppid)
            .filter(|p| *p != 0);
    }
    #[allow(unreachable_code)]
    {
        let _ = pid;
        None
    }
}

pub fn parents() -> impl Fn(u32) -> Option<u32> {
    #[cfg(windows)]
    let all: std::collections::HashMap<u32, u32> = crate::procs::processes()
        .into_iter()
        .map(|p| (p.pid, p.ppid))
        .collect();
    move |pid| {
        #[cfg(windows)]
        {
            all.get(&pid).copied().filter(|p| *p != 0)
        }
        #[cfg(not(windows))]
        {
            parent_of(pid)
        }
    }
}

pub fn descends_from(pid: u32, roots: &[u32], parent: impl Fn(u32) -> Option<u32>) -> bool {
    let mut cur = pid;
    for _ in 0..64 {
        if cur <= 1 {
            return false;
        }
        if roots.contains(&cur) {
            return true;
        }
        match parent(cur) {
            Some(p) if p != cur => cur = p,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_up_to_a_root() {
        let parent = |p: u32| match p {
            40 => Some(30),
            30 => Some(20),
            20 => Some(10),
            10 => Some(1),
            _ => None,
        };
        assert!(descends_from(40, &[20], parent));
        assert!(descends_from(20, &[20], parent));
        assert!(!descends_from(40, &[50], parent));
        assert!(!descends_from(10, &[20], parent), "a parent is no child");
        assert!(!descends_from(40, &[], parent));
        assert!(!descends_from(1, &[1], parent), "never pid 1 itself");

        assert!(!descends_from(7, &[9], |p| Some(if p == 7 {
            8
        } else {
            7
        })));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_os_tells_who_connected_and_its_parents() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let me = std::process::id();
        assert_eq!(pid_of(&a), Some(me));
        let pp = parent_of(me).expect("our parent");
        assert!(pp >= 1 && pp != me);
        assert!(descends_from(me, &[pp], parent_of));

        let mut c = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        assert_eq!(parent_of(c.id()), Some(me));
        assert!(descends_from(c.id(), &[me], parent_of));
        assert!(!descends_from(me, &[c.id()], parent_of));
        let _ = c.kill();
        let _ = c.wait();
    }
}
