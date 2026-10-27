//! A stub program written as a `/bin/sh` script, runnable on every OS.
//! On Windows a script cannot be started as a program, so a `.cmd`
//! forwards to Git for Windows' own sh.exe (found through git itself) and
//! the script, which uses forward slash paths there.

use std::path::{Path, PathBuf};

/// Git for Windows' sh.exe (never WSL's bash), from `git --exec-path`.
#[cfg(windows)]
fn git_sh() -> PathBuf {
    let out = std::process::Command::new("git")
        .arg("--exec-path")
        .output()
        .expect("git");
    let exec = PathBuf::from(
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .replace('/', "\\"),
    );
    // <root>\mingw64\libexec\git-core -> <root>\usr\bin\sh.exe
    for up in [3, 2] {
        let mut root = exec.clone();
        for _ in 0..up {
            root.pop();
        }
        let sh = root.join("usr").join("bin").join("sh.exe");
        if sh.exists() {
            return sh;
        }
    }
    PathBuf::from("sh.exe")
}

/// A path as a script may spell it (forward slashes on Windows).
pub fn sh_path(p: &Path) -> String {
    crate::fspath::plain(p).to_string_lossy().replace('\\', "/")
}

/// Write `script` (a `#!/bin/sh` text) as the program `dir/name` and
/// return what to run: the file itself on unix, `name.cmd` on Windows.
pub fn program(dir: &Path, name: &str, script: &str) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
    #[cfg(windows)]
    {
        let body = dir.join(format!("{name}.sh"));
        // LF only: sh chokes on CR.
        std::fs::write(&body, script.replace("\r\n", "\n")).unwrap();
        let cmd = dir.join(format!("{name}.cmd"));
        std::fs::write(
            &cmd,
            format!(
                "@echo off\r\n\"{}\" \"{}\" %*\r\n",
                git_sh().display(),
                sh_path(&body)
            ),
        )
        .unwrap();
        cmd
    }
}
