use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::app::App;
use crate::harness::Harness;
use crate::relink::{self, Hit, Lost, Status};

const MAX_FOLDERS: usize = 80;

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}

pub fn lost_json(l: &Lost) -> Value {
    json!({
        "conversation": l.id,
        "folder": crate::config::tilde(&l.dir),
        "why": l.why,
        "fix": "relink_conversation on this tab: search true lists where the conversation is stored; path (the project's folder now) points the tab at it",
    })
}

fn hit_json(h: &Hit) -> Value {
    let dir = Path::new(&h.cwd);
    json!({
        "folder": crate::config::tilde(dir),
        "folder_exists": !h.cwd.is_empty() && dir.is_dir(),
        "stored_at": crate::config::tilde(&h.path),
    })
}

impl App {
    pub(crate) fn conversation_lost(
        &self,
        h: Harness,
        home: &Path,
        id: &str,
        tab_dir: &Path,
        cwd: &Path,
        fell_back: bool,
    ) -> Option<Lost> {
        let short: String = id.chars().take(8).collect();
        let why = if fell_back {
            format!("the folder {} is gone", crate::config::tilde(tab_dir))
        } else if relink::status(h, home, cwd, id) == Status::Missing {
            format!(
                "conversation {short} is not stored for {}",
                crate::config::tilde(cwd)
            )
        } else {
            return None;
        };
        Some(Lost {
            id: id.to_string(),
            dir: tab_dir.to_path_buf(),
            why,
        })
    }

    pub(crate) fn mark_conversation_lost(&mut self, pane: usize, lost: Lost) {
        crate::log::info(&format!(
            "t{}: conversation {} not found ({})",
            self.panes[pane].cur().uid,
            lost.id,
            lost.why
        ));
        self.flash(format!(
            "Conversation not found for {}: relink it from the tab menu",
            crate::config::tilde(&lost.dir)
        ));
        self.panes[pane].cur_mut().mark_lost(lost);
        self.state_dirty = true;
    }

    pub(crate) fn note_conversation_lost(&mut self, s: usize, t: usize) {
        let Some(a) = self.panes[s].account else {
            return;
        };
        let tab = &self.panes[s].tabs[t];
        let Some(id) = tab.session_id.clone() else {
            return;
        };
        if !relink::screen_says_missing(&tab.screen_text()) {
            return;
        }
        let lost = Lost {
            id,
            dir: tab.cwd.clone(),
            why: format!(
                "{} says it has no such conversation",
                self.cfg.accounts[a].harness().name()
            ),
        };
        self.panes[s].tabs[t].restart_at = None;
        self.panes[s].tabs[t].crashes.clear();
        let uid = self.panes[s].tabs[t].uid;
        crate::log::info(&format!("t{uid}: {}", lost.why));
        self.flash(format!(
            "Conversation not found for {}: relink it from the tab menu",
            crate::config::tilde(&lost.dir)
        ));
        self.panes[s].tabs[t].lost = Some(lost);
    }

    fn relink_folders(&self, first: &[Hit]) -> Vec<String> {
        let mut seen: Vec<PathBuf> = vec![];
        let mut add = |p: PathBuf| {
            if p.is_dir() && !crate::guard::in_vault(&p) && !seen.iter().any(|x| x == &p) {
                seen.push(p);
            }
        };
        for h in first {
            add(PathBuf::from(&h.cwd));
        }
        for p in self.project_extra_roots() {
            add(p);
        }
        {
            let ix = self.project_index.lock().unwrap_or_else(|e| e.into_inner());
            for root in ix.roots.values() {
                for e in root.entries.iter().take(400) {
                    add(root.root.join(&e.rel));
                }
            }
        }
        seen.truncate(MAX_FOLDERS);
        seen.iter().map(|p| crate::config::tilde(p)).collect()
    }

    pub(crate) fn relink_tool(
        &mut self,
        tool: &str,
        args: &Value,
    ) -> Option<Result<Value, String>> {
        (tool == "relink_conversation").then(|| self.relink_conversation(args))
    }

    fn relink_conversation(&mut self, args: &Value) -> Result<Value, String> {
        let s = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|x| !x.is_empty())
        };
        let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
        let id = s("id").or_else(|| s("tab")).ok_or("id is required")?;
        let (sl, t) = id
            .strip_prefix('t')
            .and_then(|n| n.parse::<u64>().ok())
            .and_then(|u| self.locate_uid(u))
            .ok_or_else(|| format!("no open tab {id}"))?;
        let acct = self.panes[sl].account.ok_or("this tab has no account")?;
        let a = self.cfg.accounts[acct].clone();
        let (h, home) = (a.harness(), a.config_dir());
        let tab = &self.panes[sl].tabs[t];
        let sid = tab
            .session_id
            .clone()
            .ok_or("this tab has no conversation to relink; it has not started one")?;
        let old = tab.cwd.clone();
        let name = tab.name();
        let hits = relink::locate(h, &home, &sid);

        let Some(raw) = s("path") else {
            let lost = self.panes[sl].tabs[t].lost.as_ref().map(lost_json);
            let found: Vec<Value> = hits.iter().map(hit_json).collect();
            let say = match (found.len(), &lost) {
                (0, _) => format!(
                    "The conversation of '{name}' is not stored on this account under any folder. Ask where the project is; if it is not found, a new conversation can start there."
                ),
                (1, _) => format!(
                    "The conversation of '{name}' is stored under {}.",
                    found[0]["folder"].as_str().unwrap_or("another folder")
                ),
                (n, _) => format!("The conversation of '{name}' is stored under {n} folders."),
            };
            return Ok(ok(json!({
                "tab": id,
                "conversation": sid,
                "harness": h.name(),
                "folder": crate::config::tilde(&old),
                "lost": lost,
                "matches": found,
                "known_folders": self.relink_folders(&hits),
                "say": say,
                "next": "Ask the user where the project is now, or offer a match; then call relink_conversation again with id and path.",
            })));
        };

        let dir = relink::check_dir(raw)?;
        let mut copied = None;
        let fresh = flag("fresh");
        if !fresh && relink::status(h, &home, &dir, &sid) == Status::Missing {
            match hits.first() {
                Some(hit) if flag("copy") => {
                    let to = relink::copy_into(h, &home, hit, &sid, &dir)?;
                    copied = Some(to);
                }
                Some(hit) => {
                    let from = if hit.cwd.is_empty() {
                        crate::config::tilde(&hit.path)
                    } else {
                        crate::config::tilde(Path::new(&hit.cwd))
                    };
                    return Ok(ok(json!({
                        "needs_copy": true,
                        "from": from,
                        "to": crate::config::tilde(&dir),
                        "say": format!("The conversation is stored under {from}, not under {}. Copying it makes it resume there; the original stays where it is. Copy it?", crate::config::tilde(&dir)),
                        "next": "Ask the user exactly once. Only after a clear yes call relink_conversation again with the same id and path and copy true.",
                    })));
                }
                None => {
                    return Ok(ok(json!({
                        "not_found": true,
                        "say": format!("No conversation {} exists on this account, under any folder. A new conversation can start in {}.", sid.chars().take(8).collect::<String>(), crate::config::tilde(&dir)),
                        "next": "Only if the user wants a new conversation there, call relink_conversation again with the same id and path and fresh true.",
                    })));
                }
            }
        }

        let kind = {
            let tab = &mut self.panes[sl].tabs[t];
            tab.cwd = dir.clone();
            tab.live_cwd = None;
            tab.lost = None;
            if fresh {
                tab.set_session(None, "a lost conversation was replaced by a fresh one");
            }
            tab.relaunch_kind()
        };
        self.state_dirty = true;
        self.panes[sl].tabs[t].kill();
        self.launch_tab(sl, t, kind);
        if let Some(l) = self.panes[sl].tabs[t].lost.clone() {
            return Err(format!(
                "relinked '{name}' to {} but it still cannot resume: {}",
                crate::config::tilde(&dir),
                l.why
            ));
        }
        let place = crate::config::tilde(&dir);
        let say = match (fresh, &copied) {
            (true, _) => format!("'{name}' starts a new conversation in {place}."),
            (_, Some(_)) => format!(
                "Copied the conversation into {place} (the original stays) and resumed '{name}' there."
            ),
            _ => format!("'{name}' now points at {place} and resumes its conversation."),
        };
        Ok(ok(json!({
            "relinked": true,
            "tab": id,
            "folder": place,
            "copied": copied.map(|c| crate::config::tilde(&c)),
            "fresh": fresh,
            "say": say,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::LOCK as ENV_LOCK;
    use crate::creds::CredSource;

    const SID: &str = "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee";

    struct T {
        app: App,
        home: PathBuf,
    }

    impl Drop for T {
        fn drop(&mut self) {
            for p in &mut self.app.panes {
                p.kill_all();
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn setup(tag: &str) -> T {
        let home = std::env::temp_dir().join(format!("godterm-rl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let home = crate::fspath::canonical(&home).unwrap();
        crate::config::testing::set_home(&home);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(crate::config::Config::default(), tx);
        for a in 0..app.accounts.len() {
            app.accounts[a].login.source = Some(CredSource::Keychain);
        }
        let stub = crate::test_stub::claude(
            &home.join("bin"),
            &[
                ("sleep_s", "30".to_string()),
                ("args_to", home.join("args.txt").display().to_string()),
            ],
        );
        app.cfg.claude_bin = Some(stub.to_string_lossy().into_owned());
        app.resume_check = true;
        T { app, home }
    }

    fn transcript(t: &T, project: &Path) -> PathBuf {
        let acct = t.app.cfg.accounts[0].config_dir();
        let dir = acct
            .join("projects")
            .join(crate::state::encode_project_dir(project));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("{SID}.jsonl"));
        std::fs::write(
            &f,
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n",
                project.display()
            ),
        )
        .unwrap();
        f
    }

    #[test]
    fn a_moved_folder_is_flagged_then_relinked_by_copying_the_transcript() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut t = setup("flow");
        let old = t.home.join("was-here");
        let new = t.home.join("now-here");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let src = transcript(&t, &old);
        std::fs::remove_dir_all(&old).unwrap();

        t.app.panes[0].account = Some(0);
        t.app.panes[0].cur_mut().cwd = old.clone();
        t.app.panes[0].cur_mut().session_id = Some(SID.into());
        let uid = t.app.panes[0].cur().uid;
        let kind = t.app.panes[0].cur().relaunch_kind();
        t.app.launch(0, kind);

        let tab = &t.app.panes[0].tabs[0];
        let lost = tab.lost.clone().expect("flagged as lost");
        assert_eq!(lost.id, SID);
        assert_eq!(tab.cwd, old);
        assert!(!tab.is_running());
        let j = t.app.tab_json(0, 0);
        assert_eq!(j["conversation_lost"]["conversation"], SID, "{j}");

        let id = format!("t{uid}");
        let v = t
            .app
            .control_call("relink_conversation", &json!({"id": id}));
        assert_eq!(v["result"]["matches"][0]["folder_exists"], false, "{v}");
        assert!(v["result"]["known_folders"].is_array());

        let v = t.app.control_call(
            "relink_conversation",
            &json!({"id": id, "path": "/nope/never"}),
        );
        assert!(
            v["error"].as_str().unwrap_or("").contains("does not exist"),
            "{v}"
        );
        let v = t.app.control_call(
            "relink_conversation",
            &json!({"id": id, "path": "/Volumes/Private/x"}),
        );
        assert!(v["error"].as_str().unwrap_or("").contains("vault"), "{v}");

        let v = t.app.control_call(
            "relink_conversation",
            &json!({"id": id, "path": new.display().to_string()}),
        );
        assert_eq!(v["result"]["needs_copy"], true, "{v}");
        assert!(src.is_file());
        assert_eq!(t.app.panes[0].tabs[0].cwd, old);

        let v = t.app.control_call(
            "relink_conversation",
            &json!({"id": id, "path": new.display().to_string(), "copy": true}),
        );
        assert_eq!(v["result"]["relinked"], true, "{v}");
        assert!(src.is_file(), "the original stays");
        let tab = &t.app.panes[0].tabs[0];
        assert!(tab.lost.is_none());
        assert_eq!(tab.cwd, new);
        assert!(tab.is_running());
        assert_eq!(tab.session_id.as_deref(), Some(SID));
        let copy = t.app.cfg.accounts[0]
            .config_dir()
            .join("projects")
            .join(crate::state::encode_project_dir(&new))
            .join(format!("{SID}.jsonl"));
        assert!(copy.is_file());
    }

    #[test]
    fn nothing_stored_anywhere_offers_a_fresh_start_only_on_request() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut t = setup("fresh");
        let new = t.home.join("proj");
        std::fs::create_dir_all(&new).unwrap();
        t.app.panes[0].account = Some(0);
        t.app.panes[0].cur_mut().cwd = new.clone();
        t.app.panes[0].cur_mut().session_id = Some(SID.into());
        let uid = t.app.panes[0].cur().uid;
        let kind = t.app.panes[0].cur().relaunch_kind();
        t.app.launch(0, kind);
        assert!(t.app.panes[0].tabs[0].lost.is_some());

        let id = format!("t{uid}");
        let v = t.app.control_call(
            "relink_conversation",
            &json!({"id": id, "path": new.display().to_string()}),
        );
        assert_eq!(v["result"]["not_found"], true, "{v}");
        assert!(t.app.panes[0].tabs[0].lost.is_some());

        let v = t.app.control_call(
            "relink_conversation",
            &json!({"id": id, "path": new.display().to_string(), "fresh": true}),
        );
        assert_eq!(v["result"]["fresh"], true, "{v}");
        let tab = &t.app.panes[0].tabs[0];
        assert!(tab.lost.is_none() && tab.is_running());
    }

    #[test]
    fn a_found_conversation_launches_without_being_flagged() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut t = setup("ok");
        let dir = t.home.join("here");
        std::fs::create_dir_all(&dir).unwrap();
        transcript(&t, &dir);
        t.app.panes[0].account = Some(0);
        t.app.panes[0].cur_mut().cwd = dir.clone();
        t.app.panes[0].cur_mut().session_id = Some(SID.into());
        let kind = t.app.panes[0].cur().relaunch_kind();
        t.app.launch(0, kind);
        let tab = &t.app.panes[0].tabs[0];
        assert!(tab.lost.is_none() && tab.is_running());
    }
}
