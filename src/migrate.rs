//! One time migration from claudego (`~/.claudego`) to GodTerm
//! (`~/.godterm`).
//!
//! Logins must survive. claude names each slot's keychain item
//! `Claude Code-credentials-<sha256(CLAUDE_CONFIG_DIR)[..8]>` from the
//! exact path string it is given (a symlinked path, or the same path with
//! a trailing slash, reads as logged out). So account slot folders are NOT
//! moved: they stay in `~/.claudego/accounts/<name>` and config.toml gets
//! `config_dir = "<that exact path>"` for each of them. Everything else
//! (config, state, voice, trash, assistant, logs) moves to `~/.godterm`.
//! Keychain items are never read, copied, rewritten or deleted.
//!
//! Before anything moves, `~/.godterm/migration-backup-<date>.tar.gz` gets
//! a copy of `~/.claudego` (transcripts left out when they are large).
//! The run is logged, marked done, and safe to run again.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MARKER: &str = ".migrated-from-claudego";
/// Transcripts are left out of the backup above this total size.
const BACKUP_FULL_LIMIT: u64 = 500 * 1024 * 1024;

/// Names in the old home that stay where they are.
const KEEP: &[&str] = &[
    "accounts",
    "control.sock",
    "control.json",
    "MOVED-TO-GODTERM.txt",
    "config.toml.migrated",
];

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Backup {
        to: PathBuf,
        without_transcripts: bool,
        bytes: u64,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    /// Account `name` keeps its slot at `path` (written as config_dir).
    PinSlot {
        name: String,
        path: String,
    },
    Note(PathBuf),
    Marker(PathBuf),
    /// A file in the way is renamed first.
    SetAside {
        path: PathBuf,
        to: PathBuf,
    },
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::Backup {
                to,
                without_transcripts,
                bytes,
            } => write!(
                f,
                "back up the old folder ({:.1} MB{}) to {}",
                *bytes as f64 / 1_048_576.0,
                if *without_transcripts {
                    ", transcripts left out"
                } else {
                    ""
                },
                to.display()
            ),
            Step::Move { from, to } => write!(f, "move {} -> {}", from.display(), to.display()),
            Step::PinSlot { name, path } => write!(
                f,
                "keep account {name} at {path} (config_dir, so its login stays)"
            ),
            Step::Note(p) => write!(f, "leave a note at {}", p.display()),
            Step::Marker(p) => write!(f, "mark done ({})", p.display()),
            Step::SetAside { path, to } => write!(
                f,
                "set aside {} (made before the migration) as {}",
                path.display(),
                to.display()
            ),
        }
    }
}

pub fn old_home() -> PathBuf {
    crate::config::home_dir().join(".claudego")
}

pub fn new_home() -> PathBuf {
    crate::config::home_dir().join(".godterm")
}

fn dir_size(p: &Path) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if m.is_dir() {
        std::fs::read_dir(p)
            .map(|it| it.flatten().map(|e| dir_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        m.len()
    }
}

/// Account names in the old config.toml.
fn old_accounts(old: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(old.join("config.toml")) else {
        return vec![];
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return vec![];
    };
    doc.get("account")
        .and_then(|a| a.as_array_of_tables())
        .map(|arr| {
            arr.iter()
                .filter(|t| t.get("config_dir").is_none())
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What a migration from `old` to `new` would do; None when there is
/// nothing to do (no old install, or already done).
pub fn plan(old: &Path, new: &Path) -> Option<Vec<Step>> {
    if !old.is_dir() || new.join(MARKER).exists() {
        return None;
    }
    let mut steps = vec![];
    let total = dir_size(old);
    let date = chrono::Local::now().format("%Y%m%d-%H%M%S");
    steps.push(Step::Backup {
        to: new.join(format!("migration-backup-{date}.tar.gz")),
        without_transcripts: total > BACKUP_FULL_LIMIT,
        bytes: total,
    });
    let mut names: Vec<String> = std::fs::read_dir(old)
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    for n in names {
        if KEEP.contains(&n.as_str()) {
            continue;
        }
        let to = if n == "claudego.log" {
            new.join("godterm.log")
        } else {
            new.join(&n)
        };
        if to.exists() {
            // A config written before the migration (a default made by an
            // early command) must not win over the real one: set it aside.
            if n == "config.toml" {
                steps.push(Step::SetAside {
                    path: to.clone(),
                    to: new.join("config.toml.generated-default"),
                });
                steps.push(Step::Move {
                    from: old.join(&n),
                    to,
                });
            }
            continue; // otherwise: already there (a partial earlier run)
        }
        steps.push(Step::Move {
            from: old.join(&n),
            to,
        });
    }
    for name in old_accounts(old) {
        // The exact string claudego passed as CLAUDE_CONFIG_DIR.
        let path = old
            .join("accounts")
            .join(&name)
            .to_string_lossy()
            .into_owned();
        steps.push(Step::PinSlot { name, path });
    }
    steps.push(Step::Note(old.join("MOVED-TO-GODTERM.txt")));
    steps.push(Step::Marker(new.join(MARKER)));
    Some(steps)
}

/// Write config_dir for these accounts into config.toml (keeping comments).
fn pin_slots(config: &Path, pins: &[(String, String)]) -> Result<()> {
    if pins.is_empty() || !config.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(config)?;
    let mut doc: toml_edit::DocumentMut = text.parse().context("config.toml does not parse")?;
    if let Some(arr) = doc
        .get_mut("account")
        .and_then(|a| a.as_array_of_tables_mut())
    {
        for t in arr.iter_mut() {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            if t.get("config_dir").is_some() {
                continue;
            }
            if let Some((_, path)) = pins.iter().find(|(n, _)| *n == name) {
                t["config_dir"] = toml_edit::value(path.clone());
            }
        }
    }
    let out = doc.to_string();
    crate::config::Config::parse(&out)
        .context("config.toml would not parse after pinning slots")?;
    let tmp = config.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(tmp, config)?;
    Ok(())
}

/// Carry out a plan. `say` gets one line per step.
pub fn run(old: &Path, new: &Path, steps: &[Step], say: &mut dyn FnMut(String)) -> Result<()> {
    std::fs::create_dir_all(new)?;
    let mut pins = vec![];
    for s in steps {
        say(s.to_string());
        match s {
            Step::Backup {
                to,
                without_transcripts,
                ..
            } => {
                let parent = old.parent().context("old home has no parent")?;
                let name = old.file_name().context("old home has no name")?;
                let mut cmd = Command::new("tar");
                cmd.arg("-czf").arg(to).arg("-C").arg(parent);
                cmd.args(["--exclude", "*.sock"]);
                if *without_transcripts {
                    cmd.args(["--exclude", "*/projects/*"]);
                }
                cmd.arg(name);
                let st = cmd.status().context("running tar")?;
                if !st.success() {
                    bail!("the backup failed ({st}); nothing was moved");
                }
                let size = std::fs::metadata(to).map(|m| m.len()).unwrap_or(0);
                say(format!("  backup is {:.1} MB", size as f64 / 1_048_576.0));
            }
            Step::Move { from, to } => {
                if let Some(p) = to.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::rename(from, to).with_context(|| format!("moving {}", from.display()))?;
            }
            Step::PinSlot { name, path } => pins.push((name.clone(), path.clone())),
            Step::SetAside { path, to } => {
                std::fs::rename(path, to)
                    .with_context(|| format!("setting aside {}", path.display()))?;
            }
            Step::Note(p) => {
                std::fs::write(
                    p,
                    "claudego is now GodTerm. Its settings moved to ~/.godterm.\n\
                     The accounts/ folder stays here on purpose: each account's login is tied to\n\
                     this exact path (config_dir in ~/.godterm/config.toml). Do not move or delete it.\n",
                )?;
            }
            Step::Marker(p) => {
                pin_slots(&new.join("config.toml"), &pins)?;
                std::fs::write(p, chrono::Local::now().to_rfc3339())?;
            }
        }
    }
    Ok(())
}

/// A claudego that is still running would keep writing to the old folder.
pub fn old_running(old: &Path) -> Option<u32> {
    // Any claudego process (older builds have no control.json).
    if let Ok(o) = Command::new("pgrep").args(["-x", "claudego"]).output() {
        if let Some(pid) = String::from_utf8_lossy(&o.stdout)
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
        {
            return Some(pid);
        }
    }
    let text = std::fs::read_to_string(old.join("control.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pid = v.get("pid")?.as_u64()? as i32;
    let alive = crate::platform::kill(pid, 0) == 0;
    if !alive {
        return None;
    }
    let out = Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .contains("claudego")
        .then_some(pid as u32)
}

/// A migration is due (an old install, not migrated yet). Only meaningful
/// for the default homes (not when GODTERM_HOME is set).
pub fn pending() -> bool {
    !crate::config::dirs().explicit_home && old_home().is_dir() && !new_home().join(MARKER).exists()
}

/// An exclusive lock on `<new>/.migrate.lock`, held until dropped, so two
/// processes starting together migrate once.
pub struct Lock(std::fs::File);

impl Lock {
    pub fn take(new: &Path) -> Result<Lock> {
        std::fs::create_dir_all(new)?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(new.join(".migrate.lock"))?;
        // Blocks until the other run ends.
        if !crate::platform::lock(&f) {
            bail!("could not lock {}", new.join(".migrate.lock").display());
        }
        Ok(Lock(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        crate::platform::unlock(&self.0);
    }
}

/// Plan and run under the lock, re-checking after it is taken.
pub fn migrate_locked(old: &Path, new: &Path, say: &mut dyn FnMut(String)) -> Result<bool> {
    if plan(old, new).is_none() {
        return Ok(false);
    }
    let _lock = Lock::take(new)?;
    let Some(steps) = plan(old, new) else {
        return Ok(false);
    }; // the other run did it
    run(old, new, &steps, say)?;
    Ok(true)
}

/// On start: migrate once when there is an old install and no new one.
/// Skipped when a home is given explicitly (GODTERM_HOME / CLAUDEGO_HOME).
pub fn auto() {
    if crate::config::dirs().explicit_home {
        return;
    }
    let (old, new) = (old_home(), new_home());
    let Some(steps) = plan(&old, &new) else {
        return;
    };
    if let Some(pid) = old_running(&old) {
        eprintln!("godterm: claudego (pid {pid}) is still running; quit it first, then start GodTerm again to move your settings over.");
        std::process::exit(1);
    }
    let _ = steps;
    let mut lines = vec![];
    let res = migrate_locked(&old, &new, &mut |l| {
        eprintln!("godterm: migrating: {l}");
        lines.push(l);
    });
    for l in &lines {
        crate::log::info(&format!("migration: {l}"));
    }
    match res {
        Ok(true) => crate::log::info("migration from ~/.claudego finished"),
        Ok(false) => {}
        Err(e) => {
            crate::log::info(&format!("migration failed: {e:#}"));
            eprintln!("godterm: migration from ~/.claudego stopped: {e:#}");
        }
    }
}

/// `godterm migrate [--dry-run]`.
pub fn command(dry: bool) -> Result<()> {
    let (old, new) = (old_home(), new_home());
    let Some(steps) = plan(&old, &new) else {
        println!(
            "Nothing to migrate ({}).",
            if new.join(MARKER).exists() {
                "already done"
            } else {
                "no ~/.claudego"
            }
        );
        return Ok(());
    };
    if dry {
        println!("Would migrate {} -> {}:", old.display(), new.display());
        for s in &steps {
            println!("  {s}");
        }
        println!("Keychain items are not touched; account folders stay in place.");
        return Ok(());
    }
    if let Some(pid) = old_running(&old) {
        bail!("claudego (pid {pid}) is still running; quit it first");
    }
    let _ = steps;
    migrate_locked(&old, &new, &mut |l| println!("  {l}"))?;
    println!("Done.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gt-migrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// An old install: config with two accounts, state, voice, a log.
    fn old_install(root: &Path) -> PathBuf {
        let old = root.join(".claudego");
        for a in ["account1", "account2"] {
            std::fs::create_dir_all(old.join("accounts").join(a).join("projects/-x")).unwrap();
            std::fs::write(old.join("accounts").join(a).join(".claude.json"), "{}").unwrap();
        }
        std::fs::write(
            old.join("config.toml"),
            "# mine\nrefresh_secs = 120\n\n[[account]]\nname = \"account1\" # first\nlabel = \"Work\"\n\n[[account]]\nname = \"account2\"\n",
        )
        .unwrap();
        std::fs::write(old.join("state.json"), "{}").unwrap();
        std::fs::create_dir_all(old.join("voice")).unwrap();
        std::fs::write(old.join("voice/wake_profile.json"), "{}").unwrap();
        std::fs::write(old.join("claudego.log"), "log").unwrap();
        std::fs::write(old.join("control.json"), "{}").unwrap();
        old
    }

    #[test]
    fn fresh_install_has_nothing_to_do() {
        let r = tmp("fresh");
        assert!(plan(&r.join(".claudego"), &r.join(".godterm")).is_none());
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn migrates_and_keeps_slot_paths() {
        let r = tmp("old");
        let old = old_install(&r);
        let new = r.join(".godterm");
        let slot1 = old
            .join("accounts")
            .join("account1")
            .to_string_lossy()
            .into_owned();
        let steps = plan(&old, &new).unwrap();
        assert!(matches!(steps[0], Step::Backup { .. }));
        assert!(steps.iter().any(
            |s| matches!(s, Step::PinSlot { name, path } if name == "account1" && *path == slot1)
        ));
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, Step::Move { from, .. } if from.ends_with("accounts"))),
            "slots never move"
        );
        let mut log = vec![];
        run(&old, &new, &steps, &mut |l| log.push(l)).unwrap();
        // Moved: config, state, voice (with the wake profile), the log renamed.
        assert!(new.join("config.toml").is_file() && new.join("state.json").is_file());
        assert!(new.join("voice/wake_profile.json").is_file());
        assert!(new.join("godterm.log").is_file());
        // Kept: account slots, byte for byte the same path, and the socket info.
        assert!(old.join("accounts/account1/.claude.json").is_file());
        assert!(old.join("control.json").is_file());
        assert!(old.join("MOVED-TO-GODTERM.txt").is_file());
        // The config points each account at its old slot, comments intact.
        let cfg_text = std::fs::read_to_string(new.join("config.toml")).unwrap();
        assert!(cfg_text.contains("# mine") && cfg_text.contains("# first"));
        let cfg = crate::config::Config::parse(&cfg_text).unwrap();
        assert_eq!(
            cfg.accounts[0].config_dir().to_string_lossy(),
            slot1,
            "same string, so the same keychain item"
        );
        assert_eq!(cfg.accounts[1].config_dir(), old.join("accounts/account2"));
        // The backup is there and lists the old files.
        let backup = std::fs::read_dir(&new)
            .unwrap()
            .flatten()
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("migration-backup-")
            })
            .unwrap()
            .path();
        let list = Command::new("tar")
            .arg("-tzf")
            .arg(&backup)
            .output()
            .unwrap();
        let list = String::from_utf8_lossy(&list.stdout);
        assert!(
            list.contains(".claudego/config.toml")
                && list.contains(".claudego/accounts/account1/.claude.json"),
            "{list}"
        );
        assert!(log.iter().any(|l| l.contains("backup is")));
        // Running again does nothing.
        assert!(plan(&old, &new).is_none());
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn a_pinned_account_is_left_alone() {
        let r = tmp("pinned");
        let old = old_install(&r);
        std::fs::write(
            old.join("config.toml"),
            "[[account]]\nname = \"account1\"\nconfig_dir = \"/somewhere/else\"\n",
        )
        .unwrap();
        let steps = plan(&old, &r.join(".godterm")).unwrap();
        assert!(!steps.iter().any(|s| matches!(s, Step::PinSlot { .. })));
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn hash_input_unchanged() {
        // The keychain service name claude derives from a slot path.
        use sha2::{Digest, Sha256};
        let service = |p: &str| format!("{:x}", Sha256::digest(p.as_bytes()))[..8].to_string();
        let r = tmp("hash");
        let old = old_install(&r);
        let before = old
            .join("accounts")
            .join("account1")
            .to_string_lossy()
            .into_owned();
        let new = r.join(".godterm");
        let steps = plan(&old, &new).unwrap();
        run(&old, &new, &steps, &mut |_| {}).unwrap();
        let cfg = crate::config::Config::parse(
            &std::fs::read_to_string(new.join("config.toml")).unwrap(),
        )
        .unwrap();
        let after = cfg.accounts[0].config_dir().to_string_lossy().into_owned();
        assert_eq!(service(&before), service(&after));
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn concurrent_runs_migrate_once() {
        let r = tmp("race");
        let old = old_install(&r);
        let new = r.join(".godterm");
        let (o1, n1) = (old.clone(), new.clone());
        let (o2, n2) = (old.clone(), new.clone());
        let a = std::thread::spawn(move || migrate_locked(&o1, &n1, &mut |_| {}).unwrap());
        let b = std::thread::spawn(move || migrate_locked(&o2, &n2, &mut |_| {}).unwrap());
        let (ra, rb) = (a.join().unwrap(), b.join().unwrap());
        assert!(ra ^ rb, "exactly one run migrates ({ra}, {rb})");
        let backups = std::fs::read_dir(&new)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("migration-backup-")
            })
            .count();
        assert_eq!(backups, 1);
        assert!(new.join("config.toml").is_file() && new.join(MARKER).is_file());
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn a_default_config_written_first_does_not_win() {
        let r = tmp("early");
        let old = old_install(&r);
        let new = r.join(".godterm");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(
            new.join("config.toml"),
            "# generated default\n[[account]]\nname = \"account1\"\n",
        )
        .unwrap();
        let steps = plan(&old, &new).unwrap();
        assert!(steps.iter().any(|s| matches!(s, Step::SetAside { .. })));
        run(&old, &new, &steps, &mut |_| {}).unwrap();
        let text = std::fs::read_to_string(new.join("config.toml")).unwrap();
        assert!(
            text.contains("# mine") && text.contains("refresh_secs = 120"),
            "the real config is in place: {text}"
        );
        assert!(text.contains("config_dir"));
        assert!(new.join("config.toml.generated-default").is_file());
        assert!(!old.join("config.toml").exists());
        let _ = std::fs::remove_dir_all(r);
    }
}
