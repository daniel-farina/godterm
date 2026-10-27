use std::path::{Path, PathBuf};

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

pub fn sh_path(p: &Path) -> String {
    crate::fspath::plain(p).to_string_lossy().replace('\\', "/")
}

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
