//! Long-lived tokens in the state repo (opt in, docs/SYNC.md 2.6).
//!
//! Off unless this machine turned "Sync tokens to my other machines" on
//! (acct_tokens::Prefs::sync_all) and marked the token. Then
//! `tokens/<account>.json` holds `{account, token, created, last4, sha,
//! at_ms, machine}`, written ONLY when the state repo reads as private
//! right before the push (`gh repo view --json visibility` == PRIVATE,
//! kept 5 minutes). Not verifiable or not private: no token file is
//! written, any already in the working copy is taken out of this commit,
//! the rest of the sync goes on, and the report carries a warning.
//!
//! The newest of (this machine's token, its tombstone, the repo's entry)
//! wins, per account. A newer token from another machine goes into this
//! machine's keychain (never to disk) and is marked "from <machine>". A
//! tombstone (a synced token deleted somewhere) deletes the same token
//! here. Agent CLI logins are never part of this: only GodTerm's own
//! tokens, and only these files.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::state_repo::Report;
use super::Ctx;
use crate::acct_tokens::{self, Prefs, Store};

/// How long a private answer from GitHub is trusted.
const VIS_FOR: Duration = Duration::from_secs(300);

static VIS: Mutex<Option<(String, Instant, Option<bool>)>> = Mutex::new(None);

/// Whether the state repo is private: Some(true) only when its host says
/// so. A folder remote (tests, local mirrors) reads a `visibility` file
/// in the bare repo (PRIVATE, PUBLIC); no file: unknown.
pub fn repo_private(ctx: &Ctx) -> Option<bool> {
    let (owner, name) = (&ctx.sync().owner, &ctx.sync().state_repo);
    match ctx.remote() {
        super::git::Remote::Local { base } => {
            let f = base
                .join(owner)
                .join(format!("{name}.git"))
                .join("visibility");
            let t = std::fs::read_to_string(f).ok()?;
            Some(t.trim().eq_ignore_ascii_case("private"))
        }
        super::git::Remote::GitHub { account, .. } => {
            let key = format!("{account}:{owner}/{name}");
            if let Some((k, at, v)) = VIS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                if *k == key && at.elapsed() < VIS_FOR && v.is_some() {
                    return *v;
                }
            }
            let v = super::git::visibility(&account, &format!("{owner}/{name}"))
                .map(|s| s == "PRIVATE");
            *VIS.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, Instant::now(), v));
            v
        }
    }
}

fn read(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// One sync's token step, before the commit. `private` is asked only
/// when a token would be written.
pub fn sync_tokens(
    ctx: &Ctx,
    wc: &Path,
    r: &mut Report,
    store: &Store,
    private: &dyn Fn() -> Option<bool>,
) {
    let dir = wc.join("tokens");
    let mut prefs = Prefs::load(&ctx.app_home);
    let mut remote: BTreeMap<String, Value> = BTreeMap::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            if let Some(v) = read(&p) {
                if let Some(a) = v["account"].as_str() {
                    remote.insert(a.to_string(), v);
                }
            }
        }
    }
    let mut checked: Option<Option<bool>> = None;
    let mut is_private = || *checked.get_or_insert_with(private);
    let me = ctx.machine.name().to_string();
    let me_id = ctx.machine.id.clone();
    if !prefs.sync_all {
        // Off here: nothing goes up or comes down. A token this machine
        // wrote earlier stays until a delete or this is turned on again.
        return;
    }
    let names: BTreeSet<String> = ctx
        .cfg
        .accounts
        .iter()
        .filter(|a| a.harness() == crate::harness::Harness::Claude)
        .map(|a| a.name.clone())
        .collect();
    let mut keys: BTreeSet<String> = remote.keys().cloned().collect();
    keys.extend(prefs.sync.iter().cloned());
    keys.extend(prefs.tombstones.iter().map(|t| t.account.clone()));
    let mut changed = false;
    let mut blocked = false;
    for acct in keys {
        if !names.contains(&acct) {
            continue;
        }
        let file = dir.join(format!("{acct}.json"));
        let rem = remote.get(&acct);
        let r_at = rem.and_then(|v| v["at_ms"].as_i64()).unwrap_or(i64::MIN);
        let tomb = prefs.tombstones.iter().find(|t| t.account == acct).cloned();
        let t_at = tomb.as_ref().map(|t| t.at_ms).unwrap_or(i64::MIN);
        let local = prefs.syncs(&acct).then(|| store.get(&acct)).flatten();
        let l_at = local
            .as_ref()
            .map(|_| prefs.at.get(&acct).copied().unwrap_or(0))
            .unwrap_or(i64::MIN);
        let newest = r_at.max(t_at).max(l_at);
        if newest == i64::MIN {
            continue;
        }
        if let Some(v) = rem.filter(|_| r_at == newest) {
            let sha = v["sha"].as_str().unwrap_or("");
            if v["deleted"] == true {
                // Deleted somewhere: the same token goes here too.
                if prefs.sha.get(&acct).map(String::as_str) == Some(sha)
                    && store.get(&acct).is_some()
                    && store.delete(&acct).is_ok()
                {
                    prefs.forget(&acct);
                    changed = true;
                    r.applied.push(format!(
                        "token of {acct} deleted (deleted on {}; revoke it at claude.ai)",
                        v["machine"].as_str().unwrap_or("another machine")
                    ));
                }
                continue;
            }
            let from = v["machine"].as_str().unwrap_or("");
            let tok = v["token"].as_str().unwrap_or("");
            if v["machine_id"].as_str() != Some(me_id.as_str())
                && acct_tokens::plausible(tok)
                && prefs.sha.get(&acct).map(String::as_str) != Some(sha)
            {
                match store.put(&acct, tok) {
                    Ok(()) => {
                        prefs.saved(&acct, tok, r_at, Some(from));
                        if !prefs.sync.contains(&acct) {
                            prefs.sync.push(acct.clone());
                        }
                        changed = true;
                        r.applied
                            .push(format!("token of {acct} from {from} (in the keychain)"));
                    }
                    Err(e) => r.errors.push(format!("token of {acct}: {e}")),
                }
            }
            continue;
        }
        if t_at == newest {
            if let Some(t) = tomb {
                let v = json!({"account": acct, "deleted": true, "sha": t.sha,
                    "at_ms": t.at_ms, "machine": me, "machine_id": me_id});
                if rem != Some(&v) {
                    let _ = super::write_file(
                        &file,
                        &serde_json::to_vec_pretty(&v).unwrap_or_default(),
                    );
                }
            }
            continue;
        }
        // This machine's token is the newest.
        let Some(tok) = local else { continue };
        if is_private() != Some(true) {
            blocked = true;
            continue;
        }
        let meta = store.meta(&acct);
        let v = json!({"account": acct, "token": tok, "sha": acct_tokens::sha16(&tok),
            "created": meta.as_ref().and_then(|m| m.created.clone()),
            "last4": acct_tokens::last4(&tok), "at_ms": l_at, "machine": me, "machine_id": me_id});
        if rem != Some(&v) {
            let _ = super::write_file(&file, &serde_json::to_vec_pretty(&v).unwrap_or_default());
        }
    }
    // Never a token in a commit to a repo not known to be private.
    let any_token = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| read(&e.path()).is_some_and(|v| v["token"].is_string()));
    if (blocked || any_token) && is_private() != Some(true) {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if read(&e.path()).is_some_and(|v| v["token"].is_string()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        r.token_warning = Some(format!(
            "Tokens were not synced: {}/{} is not verified private on GitHub. Everything else synced.",
            ctx.sync().owner,
            ctx.sync().state_repo
        ));
    }
    if changed {
        let _ = prefs.save(&ctx.app_home);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tag: &str, machine: &str) -> (Ctx, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "godterm-synctok-{tag}-{machine}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            app.join("machine.json"),
            json!({"id": format!("id-{machine}"), "label": machine}).to_string(),
        )
        .unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.accounts.truncate(1);
        cfg.accounts[0].name = "work".into();
        cfg.accounts[0].harness = "claude".into();
        let mut c = Ctx::new(root.clone(), app, cfg);
        c.now_ms = 1_000;
        (c, root)
    }

    #[cfg(target_os = "macos")]
    fn files(wc: &Path) -> String {
        let mut s = String::new();
        for e in std::fs::read_dir(wc.join("tokens"))
            .into_iter()
            .flatten()
            .flatten()
        {
            s.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
        }
        s
    }

    /// Two machines through one working copy, a test keychain service of
    /// each (both removed at the end).
    #[test]
    #[cfg(target_os = "macos")]
    fn private_gates_tokens_restore_and_tombstones() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tok = crate::acct_tokens::TEST_TOKEN;
        let pid = std::process::id();
        let sa = Store::Keychain {
            service: format!("godterm-token-test-sync-a-{pid}"),
        };
        let sb = Store::Keychain {
            service: format!("godterm-token-test-sync-b-{pid}"),
        };
        let (a, ra) = ctx("gate", "mac-a");
        let (b, rb) = ctx("gate", "mac-b");
        let wc = ra.join("wc");
        std::fs::create_dir_all(&wc).unwrap();
        let _ = (sa.delete("work"), sb.delete("work"));
        if sa.put("work", tok).is_err() {
            eprintln!("keychain unavailable");
            return;
        }
        let mut pa = Prefs::load(&a.app_home);
        pa.saved("work", tok, 500, None);
        pa.sync.push("work".into());
        // Off by default: nothing goes up.
        pa.save(&a.app_home).unwrap();
        let mut r = Report::default();
        sync_tokens(&a, &wc, &mut r, &sa, &|| Some(true));
        assert!(files(&wc).is_empty(), "off unless turned on");
        pa.sync_all = true;
        pa.save(&a.app_home).unwrap();
        // Public or unknown: excluded, with a warning, nothing else lost.
        for vis in [Some(false), None] {
            let mut r = Report::default();
            sync_tokens(&a, &wc, &mut r, &sa, &|| vis);
            assert!(!files(&wc).contains("sk-ant"), "{vis:?}");
            assert!(r
                .token_warning
                .as_deref()
                .unwrap()
                .contains("not verified private"));
        }
        // Private: included.
        let mut r = Report::default();
        sync_tokens(&a, &wc, &mut r, &sa, &|| Some(true));
        assert!(files(&wc).contains(tok), "included when private");
        assert!(r.token_warning.is_none());
        // A token in the copy while the repo turned public is taken out.
        let mut r = Report::default();
        sync_tokens(&a, &wc, &mut r, &sa, &|| Some(false));
        assert!(!files(&wc).contains("sk-ant"));
        let mut r = Report::default();
        sync_tokens(&a, &wc, &mut r, &sa, &|| Some(true));
        // Machine B, sync on: restored into its keychain, marked.
        let mut pb = Prefs::load(&b.app_home);
        pb.sync_all = true;
        pb.save(&b.app_home).unwrap();
        let mut r = Report::default();
        sync_tokens(&b, &wc, &mut r, &sb, &|| Some(true));
        assert_eq!(sb.get("work").as_deref(), Some(tok), "{r:?}");
        let pb = Prefs::load(&b.app_home);
        assert_eq!(pb.from.get("work").map(String::as_str), Some("mac-a"));
        assert!(!std::fs::read_to_string(Prefs::path(&b.app_home))
            .unwrap()
            .contains("sk-ant"));
        // Deleted on B: a tombstone, and A deletes it too.
        sb.delete("work").unwrap();
        let mut pb = Prefs::load(&b.app_home);
        pb.deleted("work", 2_000);
        pb.save(&b.app_home).unwrap();
        let mut r = Report::default();
        sync_tokens(&b, &wc, &mut r, &sb, &|| Some(true));
        let f = files(&wc);
        assert!(
            f.contains("\"deleted\": true") && !f.contains("sk-ant"),
            "{f}"
        );
        let mut r = Report::default();
        sync_tokens(&a, &wc, &mut r, &sa, &|| Some(true));
        assert!(sa.get("work").is_none(), "deleted everywhere: {r:?}");
        assert!(r.applied.iter().any(|x| x.contains("revoke")));
        let _ = (sa.delete("work"), sb.delete("work"));
        let _ = std::fs::remove_dir_all(ra);
        let _ = std::fs::remove_dir_all(rb);
    }

    #[test]
    fn folder_remote_visibility_file() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut c, root) = ctx("vis", "m");
        let base = root.join("remote");
        c.cfg.sync.remote = format!("file://{}", base.display());
        c.cfg.sync.owner = "me".into();
        c.cfg.sync.state_repo = "state".into();
        let d = base.join("me/state.git");
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(repo_private(&c), None);
        std::fs::write(d.join("visibility"), "PUBLIC").unwrap();
        assert_eq!(repo_private(&c), Some(false));
        std::fs::write(d.join("visibility"), "PRIVATE\n").unwrap();
        assert_eq!(repo_private(&c), Some(true));
        let _ = std::fs::remove_dir_all(root);
    }
}
