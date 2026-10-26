//! `godterm sync plan`: a dry run. What would be synced, how big, what is
//! left out and why, which projects are not backed up. Reads only the
//! allowlist; writes nothing.

use serde::Serialize;
use std::collections::BTreeMap;

use super::{collect, Ctx};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Plan {
    pub config: bool,
    pub stores: BTreeMap<String, usize>,
    pub files: usize,
    pub file_bytes: u64,
    pub env_projects: usize,
    pub env_files: usize,
    pub env_not_ignored: Vec<String>,
    pub sessions: usize,
    pub hot: usize,
    pub session_bytes: u64,
    /// Bytes not pushed yet (the whole of it on a first sync).
    pub new_bytes: u64,
    pub left_out: Vec<String>,
    pub vault_mentions: usize,
    pub batches: u64,
    pub projects_not_backed_up: Vec<String>,
}

pub fn plan(ctx: &Ctx) -> Plan {
    let c = collect::collect(ctx);
    let mut p = Plan {
        config: c.home_files.contains_key("config.toml"),
        ..Default::default()
    };
    for s in super::merge::STORES {
        let v: serde_json::Value = std::fs::read(ctx.app_home.join(s.file))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let n = v[s.list].as_array().map(|a| a.len()).unwrap_or(0);
        p.stores.insert(s.name.to_string(), n);
    }
    p.files = c.files.len() + c.histories.len();
    p.file_bytes = c
        .files
        .iter()
        .map(|f| &f.path)
        .chain(c.histories.iter().map(|(_, h)| h))
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum();
    let map = ctx.paths();
    for root in super::env::project_roots(ctx) {
        let f = super::env::env_files(&root, &ctx.never);
        if !f.is_empty() {
            p.env_projects += 1;
            p.env_files += f.len();
        }
        for (rel, _) in &f {
            if super::env::ignored(&root, rel) == Some(false) {
                p.env_not_ignored
                    .push(format!("{}/{rel}", map.to_portable(&root)));
            }
        }
        let w = super::projects::check(&root);
        if !w.is_empty() {
            p.projects_not_backed_up
                .push(format!("{}: {}", map.to_portable(&root), w.join(", ")));
        }
    }
    let ledger = super::state_repo::load_ledger(ctx);
    for s in &c.sessions {
        if !ctx.sync().include_sessions.contains(&s.id)
            && s.files().iter().any(|(_, f)| collect::mentions_vault(f, 0))
        {
            p.vault_mentions += 1;
            p.left_out
                .push(format!("{}: mentions {}", s.id, super::VAULT));
            continue;
        }
        p.sessions += 1;
        p.hot += s.hot as usize;
        p.session_bytes += s.bytes;
        for (name, f) in s.files() {
            let len = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
            let done = ledger
                .get(&format!("{}/{}/{name}", s.account, s.id))
                .map(|l| l.chunks.iter().map(|c| c.len).sum::<u64>())
                .unwrap_or(0);
            p.new_bytes += len.saturating_sub(done);
        }
    }
    p.left_out
        .extend(c.skipped.iter().map(|(a, b)| format!("{a}: {b}")));
    p.batches = p.new_bytes.div_ceil(super::state_repo::CYCLE_BYTES).max(1);
    p
}

pub fn mb(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    }
}

impl Plan {
    pub fn text(&self) -> String {
        let mut s = String::from("Sync plan (dry run, nothing written):\n");
        s += &format!(
            "  config: {}\n",
            if self.config {
                "portable keys of config.toml"
            } else {
                "none yet"
            }
        );
        for (k, n) in &self.stores {
            s += &format!("  {k}: {n} entries\n");
        }
        s += &format!(
            "  files (account settings, skills, memory, assistant history): {} ({})\n",
            self.files,
            mb(self.file_bytes)
        );
        s += &format!(
            "  .env: {} files in {} projects\n",
            self.env_files, self.env_projects
        );
        for e in &self.env_not_ignored {
            s += &format!("    warning: {e} is not ignored by its project's git\n");
        }
        s += &format!(
            "  conversations: {} ({} hot), {} in all, {} to upload ({} sync{})\n",
            self.sessions,
            self.hot,
            mb(self.session_bytes),
            mb(self.new_bytes),
            self.batches,
            if self.batches == 1 { "" } else { "s" }
        );
        if !self.left_out.is_empty() {
            s += &format!("  left out: {}\n", self.left_out.len());
            for l in self.left_out.iter().take(10) {
                s += &format!("    {l}\n");
            }
        }
        if !self.projects_not_backed_up.is_empty() {
            s += "  projects not backed up (GodTerm never commits or pushes them for you):\n";
            for l in self.projects_not_backed_up.iter().take(30) {
                s += &format!("    {l}\n");
            }
        }
        s
    }
}
