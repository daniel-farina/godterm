use anyhow::{bail, Result};

use super::{restore, state_repo, Ctx};

const HELP: &str = "\
godterm sync: back up this machine's GodTerm state to private GitHub repos

    godterm sync init [--owner O] [--account A] [--remote file:///DIR]
                        first machine: create OWNER/godterm-state (private,
                        after a typed yes) and push the first backup
    godterm sync clone OWNER/REPO [--label NAME] [--tabs MACHINE] [--account A]
                        [--remote file:///DIR]
                        new machine: restore config, accounts (no logins),
                        jobs, archive, projects, .env files, recent
                        conversations, and optionally another machine's tabs
    godterm sync status    the last sync, what waits, warnings
    godterm sync now       sync now
    godterm sync plan      dry run: what would be synced and how big
    godterm sync doctor    check git, gh logins, the repos, paths
    godterm sync get ID    place one conversation from the backup here

Project repos are never committed to or pushed by the sync: projects with
no GitHub repo, unpushed commits or uncommitted work are listed as not
backed up. Nothing in /Volumes/Private is ever read or synced.";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub fn command(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    if sub == "git-credential" {
        return super::git::credential_command(&args[1..]);
    }
    if matches!(sub, "help" | "-h" | "--help") {
        println!("{HELP}");
        return Ok(());
    }
    let ctx = Ctx::from_env()?;
    run(&ctx, sub, &args[1..], &restore::ask_yes)
}

pub fn run(ctx: &Ctx, sub: &str, rest: &[String], yes: &dyn Fn(&str) -> bool) -> Result<()> {
    match sub {
        "init" => {
            let mut sc = ctx.cfg.sync.clone();
            if let Some(o) = flag(rest, "--owner") {
                sc.account = o.clone();
                sc.owner = o;
            }
            if let Some(a) = flag(rest, "--account") {
                sc.account = a;
            }
            if let Some(r) = flag(rest, "--remote") {
                sc.remote = r;
            }
            let rep = restore::init(ctx, sc, yes)?;
            print_report(ctx, &rep);
        }
        "clone" => {
            let Some(full) = rest
                .iter()
                .find(|a| !a.starts_with("--") && a.contains('/'))
            else {
                bail!("usage: godterm sync clone OWNER/REPO");
            };
            let o = restore::CloneOpts {
                label: flag(rest, "--label"),
                tabs_from: flag(rest, "--tabs"),
                account: flag(rest, "--account"),
                remote: flag(rest, "--remote"),
            };
            let c = restore::clone(ctx, full, &o)?;
            println!("Restored from {full}.");
            for p in &c.projects {
                println!("  project {p}");
            }
            for p in &c.placed {
                println!("  conversation {p}");
            }
            if c.tabs > 0 {
                println!(
                    "  {} tabs to start with (they resume when GodTerm starts)",
                    c.tabs
                );
            }
            for m in &c.missing_paths {
                println!(
                    "  warning: {m} has no folder here: add it under [sync.paths] in config.toml"
                );
            }
            println!("Logins (never copied):");
            for l in &c.logins {
                println!("  {l}");
            }
            let ctx2 = restore::reload(ctx)?;
            print_report(&ctx2, &c.report);
            for (ok, line) in restore::doctor(&ctx2) {
                println!("  {} {line}", if ok { "ok  " } else { "todo" });
            }
        }
        "now" => {
            if !ctx.sync().enabled {
                bail!("sync is off here: run godterm sync init (first machine) or godterm sync clone OWNER/REPO");
            }
            let rep = state_repo::sync_now(ctx)?;
            print_report(ctx, &rep);
        }
        "status" => match state_repo::Report::load(ctx) {
            Some(r) => print_report(ctx, &r),
            None => println!(
                "No sync yet{}.",
                if ctx.sync().enabled {
                    ""
                } else {
                    " (sync is off: godterm sync init or clone)"
                }
            ),
        },
        "plan" => println!("{}", super::plan::plan(ctx).text()),
        "doctor" => {
            for (ok, line) in restore::doctor(ctx) {
                println!("{} {line}", if ok { "ok  " } else { "todo" });
            }
        }
        "get" => {
            let Some(id) = rest.first() else {
                bail!("usage: godterm sync get SESSION_ID");
            };
            let wc = state_repo::open_wc(ctx)?;
            let m = state_repo::all_manifests(&wc)
                .into_iter()
                .find(|m| m.session.starts_with(id.as_str()))
                .ok_or_else(|| anyhow::anyhow!("no conversation {id} in the backup"))?;
            let p = state_repo::place_session(ctx, &m)?;
            println!("{} is at {}", m.session, crate::config::tilde(&p));
        }
        _ => {
            println!("{HELP}");
        }
    }
    Ok(())
}

pub fn report_lines(ctx: &Ctx, r: &state_repo::Report) -> Vec<String> {
    let mut v = vec![];
    let s = ctx.sync();
    v.push(format!(
        "State repo {}/{}: {}",
        s.owner,
        s.state_repo,
        if r.ok {
            format!("synced {}", super::ago(r.at_ms, ctx.now_ms))
        } else {
            format!(
                "last try {} failed: {}",
                super::ago(r.at_ms, ctx.now_ms),
                r.error.clone().unwrap_or_default()
            )
        }
    ));
    if r.waiting > 0 {
        v.push(format!("{} changes waiting to be pushed", r.waiting));
    }
    v.push(format!(
        "Conversations: {} synced, {} pushed this time ({}), {} only in the backup{}",
        r.sessions,
        r.sessions_pushed,
        super::plan::mb(r.bytes_pushed),
        r.remote_only,
        if r.more { ", more to upload" } else { "" }
    ));
    if r.extended > 0 {
        v.push(format!("{} extended from other machines", r.extended));
    }
    for f in &r.forked {
        v.push(format!("forked (diverged, never merged): {f}"));
    }
    for h in &r.held {
        v.push(format!("held while open in a tab here: {h}"));
    }
    for m in &r.machines {
        v.push(format!(
            "machine {} ({}), seen {}",
            m["label"].as_str().unwrap_or("?"),
            m["version"].as_str().unwrap_or("?"),
            m["seen"].as_str().unwrap_or("?")
        ));
    }
    for sh in &r.shards {
        v.push(format!(
            "{}: {}{}",
            sh["repo"].as_str().unwrap_or(""),
            super::plan::mb(sh["bytes"].as_u64().unwrap_or(0)),
            if sh["sealed"] == true {
                " (sealed)"
            } else {
                ""
            }
        ));
    }
    if r.secret_warnings > 0 {
        v.push(format!(
            "{} likely secrets in pushed transcripts (warning only)",
            r.secret_warnings
        ));
    }
    if let Some(w) = &r.token_warning {
        v.push(format!("warning: {w}"));
    }
    for j in &r.new_jobs_paused {
        v.push(format!(
            "new job from another machine, paused until you turn it on: {j}"
        ));
    }
    for e in &r.env_kept_both {
        v.push(format!(".env kept both: {e}"));
    }
    for e in &r.env_not_ignored {
        v.push(format!("warning: {e} is not ignored by git"));
    }
    for p in &r.projects {
        v.push(format!(
            "not backed up: {}: {}",
            p.path,
            p.warnings.join(", ")
        ));
    }
    for x in &r.refused {
        v.push(format!("refused from the backup: {x}"));
    }
    for e in r.errors.iter().take(10) {
        v.push(format!("error: {e}"));
    }
    v
}

fn print_report(ctx: &Ctx, r: &state_repo::Report) {
    for l in report_lines(ctx, r) {
        println!("{l}");
    }
}
