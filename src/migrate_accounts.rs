//! `godterm migrate-accounts`: move account slots that still live outside
//! `~/.godterm` (in `~/.claudego/accounts`) to `~/.godterm/accounts/<name>`,
//! carrying each login over.
//!
//! claude names a slot's keychain item from the exact CLAUDE_CONFIG_DIR
//! string (`Claude Code-credentials-<sha256(path)[..8]>`), so a moved slot
//! needs its item under the new name. Per account, with nothing running
//! on that slot:
//!
//! 1. note who is logged in (`claude auth status --json`, read only);
//! 2. read the old item's secret into memory (never logged, printed or
//!    written to disk);
//! 3. rename the folder (same volume);
//! 4. write the new item the way claude itself does (`security -i`, hex on
//!    stdin, so the secret is never on a command line);
//! 5. point config.toml's config_dir at the new path (comments kept);
//! 6. check `claude auth status` on the new path: logged in, same email;
//! 7. only then delete the old item.
//!
//! Any failure rolls back: the folder moves back, config_dir is restored,
//! and only the new item (if made) is deleted.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::creds::keychain_service;

/// Keychain operations, behind a trait so tests never touch the real one.
pub trait Keychain {
    fn read(&self, service: &str) -> Result<Option<String>>;
    fn write(&self, service: &str, secret: &str) -> Result<()>;
    fn delete(&self, service: &str) -> Result<()>;
    fn exists(&self, service: &str) -> bool;
}

/// The login keychain through `/usr/bin/security`.
pub struct SystemKeychain;

fn user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "claude-code-user".into())
}

/// Longest `security -i` line claude itself sends (it falls back to argv
/// beyond; we refuse instead, so the secret never reaches a command line).
const STDIN_LIMIT: usize = 4000;

impl Keychain for SystemKeychain {
    fn read(&self, service: &str) -> Result<Option<String>> {
        let out = Command::new("security")
            .args(["find-generic-password", "-a", &user(), "-s", service, "-w"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .context("running security")?;
        if !out.status.success() {
            return Ok(None);
        }
        let s = String::from_utf8(out.stdout).context("keychain item is not text")?;
        let s = s.trim_end_matches('\n');
        // `-w` prints binary data as hex; claude stores JSON text.
        if s.starts_with('{') {
            return Ok(Some(s.to_string()));
        }
        let bytes = hex_decode(s).context("keychain item is neither JSON nor hex")?;
        Ok(Some(
            String::from_utf8(bytes).context("keychain item is not UTF-8")?,
        ))
    }

    fn write(&self, service: &str, secret: &str) -> Result<()> {
        use std::io::Write;
        let hex: String = secret
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let line = format!(
            "add-generic-password -U -a \"{}\" -s \"{service}\" -X \"{hex}\"\n",
            user()
        );
        if line.len() > STDIN_LIMIT {
            bail!("the login is too large to pass on stdin ({} bytes); not writing it on a command line", secret.len());
        }
        let mut child = Command::new("security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("running security -i")?;
        child
            .stdin
            .take()
            .context("no stdin")?
            .write_all(line.as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() || !self.exists(service) {
            bail!("security could not add {service}");
        }
        Ok(())
    }

    fn delete(&self, service: &str) -> Result<()> {
        let st = Command::new("security")
            .args(["delete-generic-password", "-a", &user(), "-s", service])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !st.success() {
            bail!("security could not delete {service}");
        }
        Ok(())
    }

    fn exists(&self, service: &str) -> bool {
        Command::new("security")
            .args(["find-generic-password", "-a", &user(), "-s", service])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Who is logged in on a config dir: (logged in, email).
pub type AuthCheck<'a> = &'a dyn Fn(&str) -> Result<(bool, Option<String>)>;
/// What is using a slot right now (a process description), if anything.
pub type InUse<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `claude auth status --json` with CLAUDE_CONFIG_DIR set (read only).
pub fn claude_auth(claude: &str) -> impl Fn(&str) -> Result<(bool, Option<String>)> + '_ {
    move |dir: &str| {
        let mut cmd = Command::new(claude);
        cmd.args(["auth", "status", "--json"])
            .env("CLAUDE_CONFIG_DIR", dir)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let out = crate::creds::output_with_timeout(cmd, std::time::Duration::from_secs(30))
            .context("claude auth status timed out")?;
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).context("claude auth status gave no JSON")?;
        Ok((
            v["loggedIn"].as_bool().unwrap_or(false),
            v["email"].as_str().map(str::to_string),
        ))
    }
}

/// A godterm, or a claude on this slot, that is running (from `ps` with
/// environments, so CLAUDE_CONFIG_DIR shows).
pub fn system_in_use(dir: &str) -> Option<String> {
    let me = std::process::id();
    let out = Command::new("ps")
        .args(["-axeww", "-o", "pid=,command="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let needle = format!("CLAUDE_CONFIG_DIR={dir}");
    for l in text.lines() {
        let l = l.trim_start();
        let (pid, rest) = l.split_once(' ').unwrap_or((l, ""));
        if pid.parse::<u32>().ok() == Some(me) {
            continue;
        }
        let cmd = rest.split(" CLAUDE_").next().unwrap_or(rest);
        let exe = cmd.split_whitespace().next().unwrap_or("");
        let is_godterm = exe.ends_with("/godterm") || exe == "godterm";
        let ours = rest
            .split_whitespace()
            .any(|w| w == needle || w.starts_with(&format!("{needle}/")));
        if is_godterm && !cmd.contains(" migrate-accounts") || ours {
            return Some(format!("pid {pid}: {}", crate::sessions::snippet(cmd, 80)));
        }
    }
    None
}

/// One slot to move.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotMove {
    pub name: String,
    pub from: String,
    pub to: String,
    pub old_service: String,
    pub new_service: String,
}

/// Slots outside `home` (the GodTerm home), from config.toml.
pub fn plan(cfg: &crate::config::Config, home: &Path) -> Vec<SlotMove> {
    cfg.accounts
        .iter()
        .filter_map(|a| {
            let from = a.config_dir().to_string_lossy().into_owned();
            let to = home
                .join("accounts")
                .join(&a.name)
                .to_string_lossy()
                .into_owned();
            (!Path::new(&from).starts_with(home) && from != to).then(|| SlotMove {
                name: a.name.clone(),
                old_service: keychain_service(&from),
                new_service: keychain_service(&to),
                from,
                to,
            })
        })
        .collect()
}

/// Set (or with None remove) an account's config_dir in config.toml.
fn set_config_dir(config: &Path, name: &str, dir: Option<&str>) -> Result<()> {
    let text = std::fs::read_to_string(config)?;
    let mut doc: toml_edit::DocumentMut = text.parse().context("config.toml does not parse")?;
    let arr = doc
        .get_mut("account")
        .and_then(|a| a.as_array_of_tables_mut())
        .context("no [[account]] in config.toml")?;
    let t = arr
        .iter_mut()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
        .with_context(|| format!("no account {name} in config.toml"))?;
    match dir {
        Some(d) => {
            t.insert("config_dir", toml_edit::value(d));
        }
        None => {
            t.remove("config_dir");
        }
    }
    let out = doc.to_string();
    crate::config::Config::parse(&out).context("config.toml would not parse")?;
    let tmp = config.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, config)?;
    Ok(())
}

/// In `<slot>/.claude.json`, absolute references to the slot itself.
fn patch_claude_json(slot: &Path, from: &str, to: &str) -> Result<bool> {
    let p = slot.join(".claude.json");
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Ok(false);
    };
    // As JSON writes them: an opening quote and the escaped path (Windows
    // paths have their backslashes doubled), then a closing quote or a
    // separator (/, or an escaped backslash).
    let quoted = |s: &str| {
        let mut q = serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""));
        q.pop();
        q
    };
    let (f, t) = (quoted(from), quoted(to));
    let ends = ["\"", "/", "\\\\"];
    if !ends.iter().any(|e| text.contains(&format!("{f}{e}"))) {
        return Ok(false);
    }
    let mut out = text.clone();
    for e in ends {
        out = out.replace(&format!("{f}{e}"), &format!("{t}{e}"));
    }
    serde_json::from_str::<serde_json::Value>(&out).context(".claude.json would not parse")?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, &p)?;
    Ok(true)
}

/// Move one slot. On error everything is put back.
pub fn move_slot(
    m: &SlotMove,
    config: &Path,
    kc: &dyn Keychain,
    auth: AuthCheck,
    in_use: InUse,
    say: &mut dyn FnMut(String),
) -> Result<()> {
    if let Some(p) = in_use(&m.from) {
        bail!("{} is in use ({p}); quit it first", m.name);
    }
    if Path::new(&m.to).exists() {
        bail!("{} already exists", m.to);
    }
    if !Path::new(&m.from).is_dir() {
        bail!("{} is not a folder", m.from);
    }
    let (was_in, email) =
        auth(&m.from).with_context(|| format!("checking {} before the move", m.name))?;
    let secret = kc.read(&m.old_service)?;
    if was_in && secret.is_none() && !Path::new(&m.from).join(".credentials.json").is_file() {
        bail!(
            "{} is logged in but its keychain item {} cannot be read",
            m.name,
            m.old_service
        );
    }
    if kc.exists(&m.new_service) {
        bail!(
            "a keychain item {} already exists; not overwriting it",
            m.new_service
        );
    }
    if let Some(parent) = Path::new(&m.to).parent() {
        std::fs::create_dir_all(parent)?;
    }
    // From here on, undo on failure.
    std::fs::rename(&m.from, &m.to).with_context(|| format!("moving {} to {}", m.from, m.to))?;
    say(format!("{}: moved {} -> {}", m.name, m.from, m.to));
    let mut made_item = false;
    let res = (|| -> Result<()> {
        if let Some(s) = &secret {
            kc.write(&m.new_service, s)?;
            made_item = true;
            say(format!("{}: login copied to {}", m.name, m.new_service));
        }
        set_config_dir(config, &m.name, Some(&m.to))?;
        if patch_claude_json(Path::new(&m.to), &m.from, &m.to)? {
            say(format!(
                "{}: updated paths to the slot in .claude.json",
                m.name
            ));
        }
        let (now_in, now_email) = auth(&m.to).context("checking the login after the move")?;
        if now_in != was_in || now_email != email {
            bail!(
                "after the move {} reads as {} ({:?}), before it was {} ({:?})",
                m.name,
                if now_in { "logged in" } else { "logged out" },
                now_email,
                if was_in { "logged in" } else { "logged out" },
                email
            );
        }
        say(format!(
            "{}: verified, {} as {}",
            m.name,
            if now_in { "logged in" } else { "logged out" },
            now_email.as_deref().unwrap_or("-")
        ));
        Ok(())
    })();
    match res {
        Ok(()) => {
            if secret.is_some() {
                kc.delete(&m.old_service)?;
                say(format!(
                    "{}: removed the old item {}",
                    m.name, m.old_service
                ));
            }
            Ok(())
        }
        Err(e) => {
            // Roll back: folder, config_dir, and only the item we made.
            let _ = patch_claude_json(Path::new(&m.to), &m.to, &m.from);
            let back = std::fs::rename(&m.to, &m.from);
            let cfg = set_config_dir(config, &m.name, Some(&m.from));
            if made_item {
                let _ = kc.delete(&m.new_service);
            }
            say(format!(
                "{}: rolled back ({})",
                m.name,
                if back.is_ok() && cfg.is_ok() {
                    "folder, config and keychain restored"
                } else {
                    "PARTIAL, check by hand"
                }
            ));
            Err(e)
        }
    }
}

/// The command.
pub fn command(dry: bool) -> Result<()> {
    let home = crate::config::app_home();
    let config = crate::config::Config::path();
    let cfg = crate::config::Config::load_or_init()?;
    let moves = plan(&cfg, &home);
    if moves.is_empty() {
        println!(
            "every account already lives in {}",
            home.join("accounts").display()
        );
        return Ok(());
    }
    for m in &moves {
        println!(
            "{}: {} -> {}\n    keychain {} -> {}",
            m.name, m.from, m.to, m.old_service, m.new_service
        );
    }
    if dry {
        if let Some(p) = moves.iter().find_map(|m| system_in_use(&m.from)) {
            println!("would stop: in use ({p})");
        }
        println!("dry run: nothing changed");
        return Ok(());
    }
    let claude = cfg.claude_bin();
    let auth = claude_auth(&claude);
    let mut say = |s: String| {
        println!("{s}");
        crate::log::info(&format!("migrate-accounts: {s}"));
    };
    for m in &moves {
        if let Err(e) = move_slot(m, &config, &SystemKeychain, &auth, &system_in_use, &mut say) {
            say(format!("stopped at {}: {e:#}", m.name));
            bail!(
                "stopped; {} rolled back, the accounts before it are moved",
                m.name
            );
        }
    }
    // The old accounts folder, if empty now.
    let old = crate::migrate::old_home().join("accounts");
    if old.is_dir()
        && std::fs::read_dir(&old)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(&old);
        say(format!("removed the empty {}", old.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Default)]
    struct FakeKc {
        items: RefCell<HashMap<String, String>>,
        fail_write: bool,
    }

    impl Keychain for FakeKc {
        fn read(&self, s: &str) -> Result<Option<String>> {
            Ok(self.items.borrow().get(s).cloned())
        }
        fn write(&self, s: &str, v: &str) -> Result<()> {
            if self.fail_write {
                bail!("locked");
            }
            self.items.borrow_mut().insert(s.into(), v.into());
            Ok(())
        }
        fn delete(&self, s: &str) -> Result<()> {
            self.items
                .borrow_mut()
                .remove(s)
                .map(|_| ())
                .context("no item")
        }
        fn exists(&self, s: &str) -> bool {
            self.items.borrow().contains_key(s)
        }
    }

    fn setup(tag: &str) -> (PathBuf, PathBuf, PathBuf, crate::config::Config) {
        let root = std::env::temp_dir().join(format!("godterm-macct-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("old").join("accounts").join("account1");
        let home = root.join("home");
        std::fs::create_dir_all(old.join("projects")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        // Built with the JSON and TOML writers, so Windows paths (with
        // backslashes) are escaped as the real files have them.
        let cj = serde_json::json!({"installMethod": "native", "cache": format!("{}/cache", old.display()), "projects": {"/Users/x/code": {}}});
        std::fs::write(old.join(".claude.json"), cj.to_string()).unwrap();
        let config = home.join("config.toml");
        std::fs::write(&config, format!("# my comment\n[[account]]\nname = \"account1\"\nlabel = \"Work\"\nconfig_dir = {}\n", toml_edit::Value::from(old.display().to_string()))).unwrap();
        let cfg = crate::config::Config::parse(&std::fs::read_to_string(&config).unwrap()).unwrap();
        (root, home, config, cfg)
    }

    /// Windows paths go into config.toml and .claude.json escaped, and
    /// come back as they were.
    #[test]
    fn windows_paths_are_escaped_in_toml_and_json() {
        let d = std::env::temp_dir().join(format!("godterm-macct-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let config = d.join("config.toml");
        std::fs::write(&config, "[[account]]\nname = \"a\"\n").unwrap();
        let win = r"C:\Users\alex\AppData\Local\godterm\accounts\a";
        set_config_dir(&config, "a", Some(win)).unwrap();
        let cfg = crate::config::Config::parse(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(cfg.accounts[0].config_dir.as_deref(), Some(win));
        let to = r"D:\slots\a";
        let cj = serde_json::json!({"cache": format!("{win}\\cache"), "other": "C:\\Usersx"});
        std::fs::write(d.join(".claude.json"), cj.to_string()).unwrap();
        assert!(patch_claude_json(&d, win, to).unwrap());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(v["cache"], serde_json::json!(format!("{to}\\cache")));
        assert_eq!(v["other"], serde_json::json!("C:\\Usersx"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn moves_the_slot_and_its_login() {
        let (root, home, config, cfg) = setup("ok");
        let moves = plan(&cfg, &home);
        assert_eq!(moves.len(), 1);
        let m = &moves[0];
        assert_eq!(m.old_service, keychain_service(&m.from));
        assert_eq!(
            m.new_service,
            keychain_service(&home.join("accounts").join("account1").to_string_lossy())
        );
        let kc = FakeKc::default();
        kc.items
            .borrow_mut()
            .insert(m.old_service.clone(), "{\"claudeAiOauth\":{}}".into());
        // The fake "claude auth status": logged in only where the item is.
        let auth = |d: &str| {
            Ok((
                kc.exists(&keychain_service(d)),
                kc.exists(&keychain_service(d))
                    .then(|| "a@example.com".to_string()),
            ))
        };
        let mut log = vec![];
        move_slot(m, &config, &kc, &auth, &|_| None, &mut |s| log.push(s)).unwrap();
        assert!(Path::new(&m.to).join("projects").is_dir() && !Path::new(&m.from).exists());
        assert!(
            kc.exists(&m.new_service) && !kc.exists(&m.old_service),
            "copied, then the old one removed"
        );
        let text = std::fs::read_to_string(&config).unwrap();
        let now = crate::config::Config::parse(&text).unwrap();
        assert!(
            text.starts_with("# my comment")
                && now.accounts[0].config_dir.as_deref() == Some(m.to.as_str()),
            "{text}"
        );
        let cj = std::fs::read_to_string(Path::new(&m.to).join(".claude.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&cj).unwrap();
        assert!(
            v["cache"] == serde_json::json!(format!("{}/cache", m.to))
                && v["projects"].get("/Users/x/code").is_some(),
            "slot paths patched, projects kept: {cj}"
        );
        assert!(
            log.iter().all(|l| !l.contains("claudeAiOauth")),
            "the secret is never logged"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_verify_rolls_back() {
        let (root, _home, config, cfg) = setup("rollback");
        let m = plan(&cfg, &root.join("home"))[0].clone();
        let before = std::fs::read_to_string(&config).unwrap();
        let kc = FakeKc::default();
        kc.items
            .borrow_mut()
            .insert(m.old_service.clone(), "{}".into());
        // Logged in before, a different email after: refuse.
        let n = RefCell::new(0);
        let auth = |_: &str| {
            *n.borrow_mut() += 1;
            Ok((
                true,
                Some(
                    if *n.borrow() == 1 {
                        "a@example.com"
                    } else {
                        "b@example.com"
                    }
                    .to_string(),
                ),
            ))
        };
        assert!(move_slot(&m, &config, &kc, &auth, &|_| None, &mut |_| {}).is_err());
        assert!(
            Path::new(&m.from).is_dir() && !Path::new(&m.to).exists(),
            "folder back"
        );
        let after = std::fs::read_to_string(&config).unwrap();
        let cd = |t: &str| {
            crate::config::Config::parse(t).unwrap().accounts[0]
                .config_dir
                .clone()
        };
        assert_eq!(cd(&after), cd(&before));
        assert_eq!(cd(&after).as_deref(), Some(m.from.as_str()));
        assert!(after.starts_with("# my comment"));
        assert!(
            kc.exists(&m.old_service) && !kc.exists(&m.new_service),
            "old item kept, new one removed"
        );
        // A keychain that refuses the write: same.
        let kc = FakeKc {
            fail_write: true,
            ..Default::default()
        };
        kc.items
            .borrow_mut()
            .insert(m.old_service.clone(), "{}".into());
        let auth = |_: &str| Ok((true, Some("a@example.com".to_string())));
        assert!(move_slot(&m, &config, &kc, &auth, &|_| None, &mut |_| {}).is_err());
        assert!(Path::new(&m.from).is_dir() && kc.exists(&m.old_service));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_while_in_use() {
        let (root, home, config, cfg) = setup("busy");
        let m = plan(&cfg, &home)[0].clone();
        let kc = FakeKc::default();
        let auth = |_: &str| Ok((false, None));
        let e = move_slot(
            &m,
            &config,
            &kc,
            &auth,
            &|_| Some("pid 1: claude".into()),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(format!("{e}").contains("in use"));
        assert!(Path::new(&m.from).is_dir(), "nothing moved");
        let _ = std::fs::remove_dir_all(root);
    }
}
