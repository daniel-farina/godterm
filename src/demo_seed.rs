//! The demo's made up world: six accounts (four Claude, two Grok) with
//! fake logins and usage, sixteen sessions with scenarios and history,
//! past sessions to search, and one "external" session in another
//! terminal. Written into the demo home by `godterm --demo`.
//!
//! `godterm --demo --demo-harnesses` builds the other world: one account
//! per agent CLI GodTerm runs (Claude Code, Grok Build, Codex, Cursor,
//! Antigravity, OpenCode), each with a tab of the demo agent drawn as
//! that CLI. Both worlds get made up Chrome profiles for the per-tab
//! Browser button (the real Chrome folder is never read in demo mode).

use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::demo_agent::{Flavor, Transcript};

/// One demo account.
pub struct Acct {
    pub name: &'static str,
    pub label: &'static str,
    /// The account's agent CLI (`harness::Harness::of`).
    pub harness: &'static str,
    pub color: &'static str,
    pub email: &'static str,
    /// Five hour and weekly % left (grok: weekly only).
    pub five: f64,
    pub week: f64,
}

pub const ACCOUNTS: &[Acct] = &[
    Acct {
        name: "work",
        label: "Work",
        harness: "claude",
        color: "sand",
        email: "alex@acme-corp.dev",
        five: 40.0,
        week: 31.0,
    },
    Acct {
        name: "personal",
        label: "Personal",
        harness: "claude",
        color: "sage",
        email: "alex.rivera@mailbox.dev",
        five: 63.0,
        week: 71.0,
    },
    Acct {
        name: "research",
        label: "Research",
        harness: "claude",
        color: "slate",
        email: "alex@lab.example.org",
        five: 88.0,
        week: 79.0,
    },
    Acct {
        name: "client",
        label: "Client",
        harness: "claude",
        color: "clay",
        email: "alex@northwind-studio.dev",
        five: 47.0,
        week: 36.0,
    },
    Acct {
        name: "grok-lab",
        label: "Grok Lab",
        harness: "grok",
        color: "mauve",
        email: "alex@grok-lab.dev",
        five: 0.0,
        week: 74.0,
    },
    Acct {
        name: "grok-ops",
        label: "Grok Ops",
        harness: "grok",
        color: "stone",
        email: "ops@acme-corp.dev",
        five: 0.0,
        week: 42.0,
    },
];

/// The other agent CLIs' accounts (the `--demo-harnesses` world). Their
/// CLIs manage their own logins, so they have no login or usage files.
pub const CLI_ACCOUNTS: &[Acct] = &[
    Acct {
        name: "backend",
        label: "Backend",
        harness: "codex",
        color: "sage",
        email: "alex@example.com",
        five: 0.0,
        week: 0.0,
    },
    Acct {
        name: "frontend",
        label: "Frontend",
        harness: "cursor",
        color: "clay",
        email: "alex@example.com",
        five: 0.0,
        week: 0.0,
    },
    Acct {
        name: "mobile",
        label: "Mobile",
        harness: "antigravity",
        color: "slate",
        email: "alex@example.com",
        five: 0.0,
        week: 0.0,
    },
    Acct {
        name: "local",
        label: "Local",
        harness: "opencode",
        color: "stone",
        email: "alex@example.com",
        five: 0.0,
        week: 0.0,
    },
];

impl Acct {
    pub fn grok(&self) -> bool {
        self.harness == "grok"
    }

    pub fn flavor(&self) -> Flavor {
        Flavor::of_harness(crate::harness::Harness::of(self.harness))
    }

    /// GodTerm keeps this CLI's login and usage (Claude, Grok).
    fn integrated(&self) -> bool {
        crate::harness::Harness::of(self.harness).integrated()
    }
}

/// Which world to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum World {
    /// Six Claude and Grok accounts, sixteen tabs (the default).
    Full,
    /// One account per agent CLI, a tab each.
    Harnesses,
}

/// The accounts of a world, in slot order.
pub fn accounts(world: World) -> Vec<&'static Acct> {
    match world {
        World::Full => ACCOUNTS.iter().collect(),
        World::Harnesses => {
            let by = |n: &str| ACCOUNTS.iter().find(|a| a.name == n).expect("account");
            let mut v = vec![by("work"), by("grok-lab")];
            v.extend(CLI_ACCOUNTS.iter());
            v
        }
    }
}

/// Any demo account by name.
pub fn find_account(name: &str) -> Option<&'static Acct> {
    ACCOUNTS
        .iter()
        .chain(CLI_ACCOUNTS.iter())
        .find(|a| a.name == name)
}

/// The made up Chrome profiles: (folder, name, email, has the extension
/// turned on).
pub const CHROME_PROFILES: &[(&str, &str, &str, bool)] = &[
    ("Default", "Work", "alex@example.com", true),
    ("Profile 1", "Personal", "alex.rivera@example.net", false),
    ("Profile 2", "QA", "", true),
];

/// The account that runs out during the demo.
pub const BURN_ACCOUNT: &str = "work";

/// One open tab: (account, project folder, tab name, group, pinned, accent).
struct Tab {
    acct: &'static str,
    project: &'static str,
    name: Option<&'static str>,
    group: Option<&'static str>,
    pinned: bool,
    accent: Option<&'static str>,
    /// The Chrome profile folder bound to the tab's Browser button.
    browser: Option<&'static str>,
}

const fn tab(acct: &'static str, project: &'static str) -> Tab {
    Tab {
        acct,
        project,
        name: None,
        group: None,
        pinned: false,
        accent: None,
        browser: None,
    }
}

fn tabs(world: World) -> Vec<Tab> {
    if world == World::Harnesses {
        return vec![
            Tab {
                browser: Some("Default"),
                ..tab("work", "payments-api")
            },
            tab("grok-lab", "search-indexer"),
            tab("backend", "api-gateway"),
            Tab {
                browser: Some("Profile 2"),
                ..tab("frontend", "design-system")
            },
            tab("mobile", "checkout-app"),
            tab("local", "cli-tools"),
        ];
    }
    vec![
        Tab {
            pinned: true,
            group: Some("Billing"),
            browser: Some("Default"),
            ..tab("work", "payments-api")
        },
        Tab {
            group: Some("Billing"),
            ..tab("work", "billing-dashboard")
        },
        Tab {
            accent: Some("sand"),
            ..tab("work", "landing-page")
        },
        tab("personal", "mobile-app"),
        Tab {
            accent: Some("mauve"),
            ..tab("personal", "game-prototype")
        },
        tab("personal", "chat-bot"),
        Tab {
            pinned: true,
            ..tab("research", "ml-pipeline")
        },
        Tab {
            name: Some("etl-watch"),
            ..tab("research", "data-etl")
        },
        tab("research", "docs-site"),
        Tab {
            group: Some("Release"),
            ..tab("client", "auth-service")
        },
        Tab {
            group: Some("Release"),
            ..tab("client", "infra-terraform")
        },
        tab("grok-lab", "search-indexer"),
        Tab {
            accent: Some("slate"),
            ..tab("grok-lab", "image-gen-api")
        },
        Tab {
            group: Some("Platform"),
            ..tab("grok-ops", "k8s-operator")
        },
        tab("grok-ops", "log-analyzer"),
        Tab {
            group: Some("Platform"),
            ..tab("grok-ops", "edge-cache")
        },
    ]
}

// ---------------------------------------------------------------------
// Scenario building blocks

fn think(secs: f32) -> Value {
    json!({"kind": "think", "secs": secs})
}
fn say(t: &str) -> Value {
    json!({"kind": "say", "text": t})
}
fn read(p: &str, lines: u32) -> Value {
    json!({"kind": "read", "path": p, "lines": lines})
}
fn grep(p: &str, n: u32) -> Value {
    json!({"kind": "grep", "pattern": p, "matches": n})
}
fn edit(p: &str, diff: &[&str]) -> Value {
    json!({"kind": "edit", "path": p, "diff": diff})
}
fn edit_ask(p: &str, diff: &[&str]) -> Value {
    json!({"kind": "edit", "path": p, "diff": diff, "approve": true})
}
fn write(p: &str, lines: u32) -> Value {
    json!({"kind": "write", "path": p, "lines": lines})
}
fn bash(cmd: &str, desc: &str, out: &[&str], secs: f32) -> Value {
    json!({"kind": "bash", "cmd": cmd, "desc": desc, "out": out, "secs": secs})
}
fn bash_ask(cmd: &str, desc: &str, out: &[&str], secs: f32) -> Value {
    json!({"kind": "bash", "cmd": cmd, "desc": desc, "out": out, "secs": secs, "approve": true})
}
fn todo(items: &[&str]) -> Value {
    json!({"kind": "todo", "items": items})
}
fn task(prompt: &str, steps: Vec<Value>, summary: &str) -> Value {
    json!({"prompt": prompt, "steps": steps, "summary": summary})
}

/// Every project's scenario, by folder name.
pub fn scenarios() -> Vec<(&'static str, Value)> {
    let claude = |title: &str, history: Vec<Value>, tasks: Vec<Value>, extra: Value| {
        let mut v = json!({
            "title": title, "model": "Opus 5.5", "plan": "Claude Max",
            "autorun": true, "repeat": true, "start_delay": 2.0, "idle": [3.0, 8.0], "speed": 1.0,
            "history": history, "tasks": tasks,
            "replies": [task("", vec![think(2.5), read("README.md", 88), say("Got it. I'll take care of that next."), think(2.0)], "Done. Anything else?")],
        });
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            for (k, x) in e {
                o.insert(k.clone(), x.clone());
            }
        }
        v
    };
    let grok = |title: &str, history: Vec<Value>, tasks: Vec<Value>, extra: Value| {
        let mut v = claude(title, history, tasks, extra);
        v["model"] = json!("grok-4.7-build-fast");
        v["plan"] = json!("SuperGrok Heavy");
        v
    };
    // Codex, Cursor, Antigravity, OpenCode: each CLI names its own model.
    let cli = |title: &str, history: Vec<Value>, tasks: Vec<Value>, extra: Value| {
        let mut v = claude(title, history, tasks, extra);
        v["model"] = json!("");
        v["plan"] = json!("");
        v
    };
    vec![
        ("payments-api", claude(
            "Add webhook retries with exponential backoff",
            vec![task("find out why Stripe webhooks get dropped under load", vec![], "The dispatcher gives up after one failed attempt and never retries. I'll add proper retries next.")],
            vec![
                task("add exponential backoff to the webhook retries", vec![
                    think(2.5),
                    say("I'll start with the dispatcher, then add backoff with jitter and cover it with tests."),
                    read("src/webhooks/dispatch.ts", 214),
                    grep("retryCount", 6),
                    edit("src/webhooks/dispatch.ts", &[
                        " 88   const attempt = job.attempts + 1;",
                        "-89   if (attempt > 1) return markFailed(job);",
                        "+89   if (attempt > MAX_ATTEMPTS) return deadLetter(job);",
                        "+90   const delay = Math.min(2 ** attempt * 1000, 60_000);",
                        "+91   await queue.retry(job, delay + jitter(250));",
                    ]),
                    bash("npm test -- webhooks", "Run the webhook tests", &[
                        "PASS  test/webhooks/dispatch.test.ts",
                        "  ✓ retries with exponential backoff (412 ms)",
                        "  ✓ caps the delay at 60 s (3 ms)",
                        "  ✓ dead letters after 8 attempts (21 ms)",
                        "Tests: 18 passed, 18 total",
                    ], 4.0),
                ], "Retries now back off 2s, 4s, 8s up to 60s with jitter, and dead letter after 8 attempts. All 18 webhook tests pass."),
                task("add a dead letter queue view to the admin API", vec![
                    think(2.0),
                    read("src/admin/routes.ts", 131),
                    write("src/admin/deadLetters.ts", 64),
                    edit("src/admin/routes.ts", &[
                        " 41 router.get('/admin/jobs', listJobs);",
                        "+42 router.get('/admin/dead-letters', listDeadLetters);",
                        "+43 router.post('/admin/dead-letters/:id/replay', replay);",
                    ]),
                    bash("npm test -- admin", "Run the admin API tests", &["Tests: 9 passed, 9 total"], 2.5),
                ], "Added GET /admin/dead-letters and a replay endpoint, with tests."),
            ],
            json!({"idle": [18.0, 30.0], "repeat": false, "pause_after": 1}),
        )),
        ("billing-dashboard", claude(
            "Revenue charts with drill-down",
            vec![task("sketch the revenue dashboard layout", vec![], "Laid out MRR, churn and cohort panels in a 3 column grid.")],
            vec![
                task("add drill-down from MRR to customers", vec![
                    think(3.0),
                    read("src/charts/MrrChart.tsx", 96),
                    edit("src/charts/MrrChart.tsx", &[
                        " 52   <Line data={series} />",
                        "+53   onPointClick={(p) => openDrill(p.month)}",
                    ]),
                    write("src/charts/CustomerDrill.tsx", 78),
                    bash("npm run typecheck", "Type check the app", &["tsc --noEmit", "✓ no type errors"], 3.0),
                ], "Clicking a month on the MRR chart now opens the customers behind it."),
                task("cache the revenue queries for 5 minutes", vec![
                    think(2.0),
                    grep("getRevenue", 4),
                    edit("src/api/revenue.ts", &["-12 export async function getRevenue(q) {", "+12 export const getRevenue = cached(5 * MIN, async (q) => {"]),
                    bash("npm test", "Run the tests", &["Tests: 41 passed, 41 total"], 3.0),
                ], "Revenue queries are cached for five minutes; tests pass."),
            ],
            json!({}),
        )),
        ("landing-page", claude(
            "Hero section and pricing table",
            vec![task("set up the landing page with Astro and Tailwind", vec![], "Scaffolded Astro with Tailwind and a base layout.")],
            vec![
                task("build the hero with the product screenshot", vec![
                    think(2.0),
                    write("src/components/Hero.astro", 58),
                    edit("src/pages/index.astro", &[" 3 ---", "+4 import Hero from '../components/Hero.astro';", "+12 <Hero />"]),
                    bash("npm run build", "Build the site", &["building client (vite)", "✓ 14 modules transformed", "Complete! 3 pages built in 1.84s"], 3.5),
                ], "The hero is in, with a responsive screenshot and a call to action."),
                task("make the pricing table responsive", vec![
                    think(2.0),
                    read("src/components/Pricing.astro", 112),
                    edit("src/components/Pricing.astro", &["-8 <div class=\"grid grid-cols-3\">", "+8 <div class=\"grid grid-cols-1 md:grid-cols-3 gap-6\">"]),
                    bash("npx playwright test pricing", "Run the visual tests", &["3 passed (6.1s)"], 3.0),
                ], "Pricing stacks on phones and goes 3 up from tablets."),
            ],
            json!({}),
        )),
        ("mobile-app", claude(
            "Offline sync for the notes app",
            vec![task("plan offline sync for notes", vec![], "Plan: local SQLite, a change log, and last write wins per field.")],
            vec![task("implement the sync queue with retries", vec![
                think(3.0),
                todo(&["Create the change log table", "Queue local edits", "Push and pull on reconnect", "Resolve conflicts per field"]),
                write("src/sync/queue.ts", 142),
                edit("src/db/schema.ts", &["+31 export const changes = table('changes', {", "+32   id: text().primaryKey(), op: text(), at: integer(),", "+33 });"]),
                bash("npx jest sync", "Run the sync tests", &["PASS src/sync/queue.test.ts", "Tests: 12 passed, 12 total"], 4.0),
            ], "Edits made offline queue up and sync on reconnect, with per field conflict resolution.")],
            json!({}),
        )),
        ("game-prototype", claude(
            "Physics-based grappling hook",
            vec![task("prototype a grappling hook in Bevy", vec![], "Added a rope joint between the player and the hook point.")],
            vec![task("make the rope swing feel snappier", vec![
                think(2.5),
                read("src/hook.rs", 188),
                edit("src/hook.rs", &["-64     rope.stiffness = 0.4;", "+64     rope.stiffness = 0.85;", "+65     rope.damping = 0.12;"]),
                bash("cargo run --release --example swing", "Run the swing demo", &["   Compiling game-prototype v0.3.0", "    Finished release in 9.42s", "swing test: 60 fps, max tension 412 N"], 5.0),
            ], "Stiffer rope with a bit of damping: the swing snaps instead of floating.")],
            json!({}),
        )),
        ("chat-bot", claude(
            "Slack bot that summarizes threads",
            vec![task("create a Slack bot that answers /summarize", vec![], "The bot answers /summarize with the thread's key points.")],
            vec![task("add action items to the summaries", vec![
                think(2.5),
                read("src/summarize.py", 77),
                edit("src/summarize.py", &["+41 def action_items(msgs):", "+42     return [m for m in msgs if is_task(m)]"]),
                bash("pytest -q", "Run the tests", &["........                                  [100%]", "8 passed in 1.12s"], 2.5),
            ], "Summaries now end with a list of action items and who owns them.")],
            json!({}),
        )),
        ("ml-pipeline", claude(
            "Fine-tune the ranking model",
            vec![task("prepare the click logs for fine-tuning", vec![], "Cleaned 48M click events and split them by day into train and eval.")],
            vec![task("fine-tune the ranker on last month's clicks", vec![
                think(2.0),
                read("configs/ranker.yaml", 64),
                bash("python train.py --config configs/ranker.yaml", "Train the ranking model", &[
                    "loading 41.2M examples from s3://lab-data/clicks/2026-09",
                    "epoch  1/12  loss 0.4127  val_auc 0.8614",
                    "epoch  2/12  loss 0.3871  val_auc 0.8702",
                    "epoch  3/12  loss 0.3702  val_auc 0.8768",
                    "epoch  4/12  loss 0.3590  val_auc 0.8811",
                    "epoch  5/12  loss 0.3511  val_auc 0.8843",
                    "epoch  6/12  loss 0.3453  val_auc 0.8866",
                    "epoch  7/12  loss 0.3409  val_auc 0.8881",
                    "epoch  8/12  loss 0.3377  val_auc 0.8892",
                    "epoch  9/12  loss 0.3352  val_auc 0.8899",
                    "epoch 10/12  loss 0.3334  val_auc 0.8904",
                    "epoch 11/12  loss 0.3321  val_auc 0.8907",
                    "epoch 12/12  loss 0.3313  val_auc 0.8909",
                    "saved checkpoints/ranker-2026-10-07.pt",
                ], 90.0),
            ], "Done: val AUC 0.891, up from 0.862. The checkpoint is saved.")],
            json!({"start_delay": 1.0, "idle": [6.0, 10.0]}),
        )),
        ("data-etl", claude(
            "Nightly ETL health checks",
            vec![task("summarize last night's ETL runs", vec![], "All 14 jobs succeeded; orders_daily ran 12 minutes late.")],
            vec![task("check the ETL every 15 minutes and tell me about failures", vec![
                think(2.0),
                bash("airflow dags list-runs -d nightly_etl --state failed", "List failed runs", &["no failed runs in the last 24h"], 2.0),
            ], "I'll check the ETL every 15 minutes and report any failures.")],
            json!({"repeat": false, "cron": {
                "cron": "*/15 * * * *", "human": "Every 15 minutes",
                "prompt": "check the nightly ETL runs and report failures", "every": 40.0,
                "fire": task("", vec![think(1.5), bash("airflow dags list-runs -d nightly_etl --state failed", "List failed runs", &["no failed runs in the last 15m"], 1.5)], "ETL healthy: no failed runs."),
            }}),
        )),
        ("docs-site", claude(
            "API reference from the OpenAPI spec",
            vec![task("generate API docs from openapi.yaml", vec![], "Generated 38 endpoint pages from the spec.")],
            vec![task("add code samples to every endpoint page", vec![
                think(3.0),
                read("openapi.yaml", 1240),
                edit_ask("docs/templates/endpoint.mdx", &[
                    " 18 ## Request",
                    "+19 <CodeTabs lang={['curl', 'ts', 'python']}>",
                    "+20   {samples(endpoint)}",
                    "+21 </CodeTabs>",
                ]),
                bash("npm run docs:build", "Build the docs", &["✓ 38 pages", "built in 4.2s"], 3.0),
            ], "Every endpoint page now has curl, TypeScript and Python samples.")],
            json!({"repeat": false, "start_delay": 6.0}),
        )),
        ("auth-service", claude(
            "Rotate the JWT signing keys",
            vec![task("plan a zero downtime JWT key rotation", vec![], "Plan: publish the new key in JWKS first, sign with it after 24h, retire the old one after 7 days.")],
            vec![task("run the key rotation on staging", vec![
                think(2.5),
                read("migrations/2026_10_rotate_keys.ts", 88),
                bash_ask("npm run migrate -- --env staging", "Run the key rotation migration on staging", &["migrating staging: 2026_10_rotate_keys", "✓ new key kid=k-2026-10 published to JWKS", "done in 3.1s"], 3.0),
            ], "Staging now publishes both keys; new tokens are signed with k-2026-10.")],
            json!({"repeat": false, "start_delay": 3.0}),
        )),
        ("infra-terraform", claude(
            "Add a staging EKS node pool",
            vec![task("add a spot node pool for staging", vec![], "Wrote the module; plan shows 4 resources to add.")],
            vec![task("apply the staging node pool", vec![
                think(2.0),
                bash("terraform plan -target=module.staging_pool", "Plan the change", &["Plan: 4 to add, 0 to change, 0 to destroy."], 3.0),
                bash_ask("terraform apply -target=module.staging_pool", "Apply the staging node pool", &["module.staging_pool.aws_eks_node_group.spot: Creating...", "Apply complete! Resources: 4 added, 0 changed, 0 destroyed."], 6.0),
            ], "The spot node pool is live in staging.")],
            json!({"repeat": false, "start_delay": 9.0}),
        )),
        ("search-indexer", grok(
            "Incremental reindexing with change streams",
            vec![task("design incremental reindexing", vec![], "Plan: tail the Mongo change stream and batch upserts into the index.")],
            vec![task("implement the change stream consumer", vec![
                think(2.5),
                read("src/indexer/consumer.go", 156),
                edit("src/indexer/consumer.go", &["+77 for change := range stream.Next(ctx) {", "+78     batch.Add(toDoc(change))", "+79 }"]),
                bash("go test ./indexer/...", "Run the indexer tests", &["ok    search-indexer/indexer   1.912s"], 3.0),
            ], "Changes now stream into the index in batches of 500 within a second.")],
            json!({}),
        )),
        ("image-gen-api", grok(
            "Queue image jobs with priority lanes",
            vec![task("add a job queue for image generation", vec![], "Jobs go through Redis streams now.")],
            vec![task("add priority lanes for paid users", vec![
                think(2.0),
                grep("enqueue(", 5),
                edit("src/queue.py", &["-22     stream = 'jobs'", "+22     stream = 'jobs:priority' if user.paid else 'jobs:standard'"]),
                bash("pytest tests/test_queue.py -q", "Run the queue tests", &["6 passed in 0.84s"], 2.5),
            ], "Paid users' jobs go to a priority lane, served 3 to 1.")],
            json!({}),
        )),
        ("k8s-operator", grok(
            "Operator reconcile loop for canary rollouts",
            vec![task("scaffold a canary operator with kubebuilder", vec![], "Scaffolded the Canary CRD and controller.")],
            vec![task("implement the reconcile loop", vec![
                think(3.0),
                read("controllers/canary_controller.go", 203),
                edit("controllers/canary_controller.go", &["+118 if analysis.Failed() {", "+119     return r.rollback(ctx, canary)", "+120 }"]),
                bash("make test", "Run the controller tests", &["ok    canary/controllers  6.2s  coverage 81.4%"], 4.0),
            ], "Canaries now step 10, 25, 50, 100 percent and roll back on failed analysis.")],
            json!({}),
        )),
        ("log-analyzer", grok(
            "Cluster error logs by stack trace",
            vec![task("load a day of error logs", vec![], "Loaded 2.3M error lines from Loki.")],
            vec![task("cluster the errors by stack trace", vec![
                think(2.5),
                write("analyze/cluster.py", 96),
                bash("python analyze/cluster.py --since 24h", "Cluster the errors", &["2,311,402 lines -> 37 clusters", "top: TimeoutError in payments.client (41%)"], 4.0),
            ], "37 clusters; 41% of errors are one payments client timeout.")],
            json!({}),
        )),
        ("edge-cache", grok(
            "Purge API for the edge cache",
            vec![task("design a purge API", vec![], "Purge by URL, by tag and everything, with rate limits.")],
            vec![task("implement purge by tag", vec![
                think(2.0),
                edit("src/purge.rs", &["+54 pub async fn purge_tag(tag: &str) -> Result<usize> {", "+55     let keys = index.keys_for(tag).await?;"]),
                bash("cargo test purge", "Run the purge tests", &["test purge::by_tag ... ok", "test result: ok. 7 passed"], 3.0),
            ], "Purge by tag works and clears about 12k keys a second.")],
            json!({}),
        )),
        ("api-gateway", cli(
            "Rate limit the public API",
            vec![task("add a token bucket per API key", vec![], "Added a token bucket per API key in src/limit.rs, 600 requests a minute by default.")],
            vec![
                task("return rate limit headers on every response", vec![
                    think(2.5),
                    say("I'll read the limiter, then add the headers in the middleware and cover them with a test."),
                    read("src/limit.rs", 88),
                    grep("RateLimit", 5),
                    edit("src/middleware.rs", &[
                        " 41     let verdict = limiter.check(&key).await;",
                        "+42     res.headers_mut().insert(\"RateLimit-Limit\", verdict.limit.into());",
                        "+43     res.headers_mut().insert(\"RateLimit-Remaining\", verdict.left.into());",
                    ]),
                    bash_ask("cargo test --test limits", "Run the rate limit tests", &[
                        "running 7 tests",
                        "test headers_on_every_response ... ok",
                        "test retry_after_when_empty ... ok",
                        "test result: ok. 7 passed; 0 failed",
                    ], 3.5),
                ], "Every response now carries RateLimit-Limit and RateLimit-Remaining, and a 429 adds Retry-After. All 7 tests pass."),
                task("add a per route override for /search", vec![
                    think(2.0),
                    read("config/limits.toml", 24),
                    edit("config/limits.toml", &["+9 [routes.\"/search\"]", "+10 per_minute = 120"]),
                    bash("cargo test --test limits", "Run the rate limit tests", &["test result: ok. 8 passed; 0 failed"], 2.5),
                ], "/search is limited to 120 a minute; the rest keep the default."),
            ],
            json!({"start_delay": 1.5, "idle": [8.0, 14.0]}),
        )),
        ("design-system", cli(
            "Tooltip and popover primitives",
            vec![task("audit the popover for focus traps", vec![], "The popover traps focus correctly; Escape closes it and returns focus.")],
            vec![
                task("add a Tooltip component with keyboard support", vec![
                    think(2.0),
                    say("I'll build it on the same positioning as the popover, with a delay and focus support."),
                    read("src/components/Popover.tsx", 142),
                    write("src/components/Tooltip.tsx", 84),
                    bash_ask("npm install @floating-ui/react", "Add the positioning library", &["added 3 packages in 2s"], 2.5),
                    bash("npm test -- Tooltip", "Run the tooltip tests", &["PASS src/components/Tooltip.test.tsx", "Tests: 6 passed, 6 total"], 3.0),
                ], "Tooltip is in: it opens on hover and focus, closes on Escape, and has 6 passing tests."),
                task("add tooltip stories to Storybook", vec![
                    think(2.0),
                    write("src/components/Tooltip.stories.tsx", 46),
                    bash("npm run build-storybook", "Build Storybook", &["info => Output directory: storybook-static", "built in 8.1s"], 3.0),
                ], "Storybook has Tooltip stories for every placement."),
            ],
            json!({"start_delay": 2.5, "idle": [8.0, 14.0]}),
        )),
        ("checkout-app", cli(
            "Promo codes in the checkout",
            vec![task("show the order summary before payment", vec![], "The order summary sits above the pay button with items, tax and total.")],
            vec![
                task("add a promo code field to the checkout", vec![
                    think(2.5),
                    read("lib/checkout/cart_page.dart", 210),
                    edit("lib/checkout/cart_page.dart", &[
                        " 96       OrderSummary(cart: cart),",
                        "+97       PromoCodeField(onApply: cart.applyPromo),",
                    ]),
                    bash_ask("flutter test test/checkout_test.dart", "Run the checkout tests", &[
                        "00:04 +11: All tests passed!",
                    ], 4.0),
                ], "The promo code field validates the code and updates the total. All 11 checkout tests pass."),
                task("handle expired promo codes", vec![
                    think(2.0),
                    edit("lib/checkout/promo.dart", &["+31   if (code.expiresAt.isBefore(now)) return PromoResult.expired;"]),
                    bash("flutter test test/promo_test.dart", "Run the promo tests", &["00:02 +5: All tests passed!"], 2.5),
                ], "Expired codes show a clear message instead of failing silently."),
            ],
            json!({"start_delay": 3.5, "idle": [8.0, 14.0]}),
        )),
        ("cli-tools", cli(
            "JSON output for the export command",
            vec![task("list what the export command prints today", vec![], "It prints a table only; no machine readable output yet.")],
            vec![
                task("add a --json flag to the export command", vec![
                    think(3.0),
                    read("cmd/export.go", 118),
                    edit("cmd/export.go", &[
                        "+27 \tcmd.Flags().BoolVar(&asJSON, \"json\", false, \"print JSON\")",
                        "+64 \tif asJSON { return json.NewEncoder(out).Encode(rows) }",
                    ]),
                    bash("go test ./...", "Run the tests", &["ok  \tcli-tools/cmd\t0.412s", "ok  \tcli-tools/export\t0.208s"], 3.5),
                ], "export --json prints the rows as JSON; the table stays the default."),
                task("add a golden test for the JSON output", vec![
                    think(2.0),
                    write("cmd/testdata/export.golden.json", 22),
                    bash("go test ./cmd -run Golden", "Run the golden test", &["ok  \tcli-tools/cmd\t0.198s"], 2.0),
                ], "The JSON output is pinned by a golden file."),
            ],
            json!({"start_delay": 1.0, "idle": [6.0, 10.0]}),
        )),
        ("pricing-page", claude(
            "Build a pricing page",
            vec![],
            vec![],
            json!({"autorun": false, "replies": [task("", vec![
                think(2.5),
                say("I'll build it as a component with three tiers and a monthly or yearly toggle."),
                write("src/pages/pricing.tsx", 126),
                write("src/components/PlanCard.tsx", 54),
                bash("npm run build", "Build the site", &["✓ compiled in 2.1s", "route /pricing  4.2 kB"], 3.0),
            ], "The pricing page is up at /pricing: three tiers, a yearly toggle and a FAQ.")]}),
        )),
        ("legacy-api", claude(
            "Port the cron jobs to the new scheduler",
            vec![task("list the cron jobs on the old box", vec![], "Found 9 crontab entries; 3 are dead.")],
            vec![
                task("port the six live cron jobs to the scheduler", vec![
                    think(2.5),
                    read("ops/crontab.txt", 22),
                    write("scheduler/jobs.yaml", 48),
                    bash("scheduler validate scheduler/jobs.yaml", "Validate the jobs", &["6 jobs ok"], 2.5),
                ], "The six live jobs are in scheduler/jobs.yaml and validate."),
                task("dry run tonight's schedule against staging", vec![
                    think(2.0),
                    bash("scheduler run --dry --env staging", "Dry run the schedule", &[
                        "02:00 backup-db          ok (dry)",
                        "02:30 rotate-logs        ok (dry)",
                        "03:00 sync-invoices      ok (dry)",
                        "6 of 6 jobs would run",
                    ], 4.0),
                ], "Tonight's six jobs dry run clean on staging."),
            ],
            json!({"idle": [10.0, 18.0], "repeat": false, "pause_after": 1}),
        )),
    ]
}

/// Sessions that are not open, for the sessions list and its search.
fn past_sessions() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    // (account, project, title, first prompt)
    vec![
        (
            "work",
            "payments-api",
            "Refactor invoice PDF rendering",
            "the invoice PDFs take 4 seconds, make them fast",
        ),
        (
            "work",
            "landing-page",
            "Fix the flaky checkout e2e test",
            "the checkout e2e test fails one run in five",
        ),
        (
            "personal",
            "mobile-app",
            "Port the habit tracker to SwiftUI",
            "port the habit tracker screens to SwiftUI",
        ),
        (
            "personal",
            "chat-bot",
            "Rate limit the Slack bot",
            "the bot hits Slack's rate limit at 9am",
        ),
        (
            "research",
            "ml-pipeline",
            "Benchmark vector DBs for 10M embeddings",
            "compare pgvector, Qdrant and LanceDB at 10M vectors",
        ),
        (
            "research",
            "docs-site",
            "Search for the docs with Pagefind",
            "add full text search to the docs",
        ),
        (
            "client",
            "infra-terraform",
            "Debug the pod mesh mTLS handshake failures",
            "pods in the mesh fail the mTLS handshake after the cert rotation",
        ),
        (
            "client",
            "auth-service",
            "Add passkey login",
            "add WebAuthn passkeys next to passwords",
        ),
        (
            "client",
            "infra-terraform",
            "Upgrade Postgres 15 to 17 on RDS",
            "plan the RDS upgrade from Postgres 15 to 17",
        ),
    ]
}

// ---------------------------------------------------------------------
// Writing it all

fn w(p: &Path, text: impl AsRef<[u8]>) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, text)?;
    Ok(())
}

/// Usage fixture text for an account with these % left.
pub fn usage_json(a: &Acct, five_left: f64, week_left: f64) -> String {
    let now = chrono::Utc::now();
    if a.grok() {
        let end = now + chrono::Duration::hours(3 * 24 + 15);
        let used = (100.0 - week_left).clamp(0.0, 100.0);
        return json!({"config": {
            "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": (end - chrono::Duration::days(7)).to_rfc3339(), "end": end.to_rfc3339()},
            "creditUsagePercent": used,
            "productUsage": [
                {"product": "GrokBuild", "usagePercent": (used * 0.8).round()},
                {"product": "GrokImagine", "usagePercent": (used * 0.2).round()},
                {"product": "GrokChat"}
            ],
            "isUnifiedBillingUser": true
        }})
        .to_string();
    }
    let five_reset = now + chrono::Duration::minutes(106);
    let week_reset = now + chrono::Duration::hours(3 * 24 + 15);
    json!({
        "five_hour": {"utilization": (100.0 - five_left).clamp(0.0, 100.0), "resets_at": five_reset.to_rfc3339()},
        "seven_day": {"utilization": (100.0 - week_left).clamp(0.0, 100.0), "resets_at": week_reset.to_rfc3339()},
        "seven_day_opus": {"utilization": ((100.0 - week_left) * 0.6).clamp(0.0, 100.0), "resets_at": week_reset.to_rfc3339()},
        "model_scoped": [{"display_name": "Fable", "utilization": ((100.0 - week_left) * 0.3).round(), "resets_at": week_reset.to_rfc3339()}],
        "extra_usage": {"is_enabled": false}
    })
    .to_string()
}

pub struct Paths {
    pub app: PathBuf,
    pub bin: PathBuf,
    pub work: PathBuf,
    pub usage: PathBuf,
    pub main_claude: PathBuf,
    pub main_grok: PathBuf,
}

impl Paths {
    pub fn of(home: &Path) -> Paths {
        Paths {
            app: home.join("app"),
            bin: home.join("bin"),
            work: home.join("code"),
            usage: home.join("usage"),
            main_claude: home.join("main").join("claude"),
            main_grok: home.join("main").join("grok"),
        }
    }
}

/// Link (or copy) the running godterm as the demo's agent CLIs
/// (`bin/claude`, `bin/grok`, `bin/codex`, `bin/cursor-agent`, `bin/agy`,
/// `bin/opencode`).
/// The path of a demo agent's program (a Windows one is `name.exe`).
fn agent_bin(p: &Paths, f: Flavor) -> String {
    let name = f.bin();
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    p.bin.join(file).to_string_lossy().into_owned()
}

fn link_agents(p: &Paths) -> Result<()> {
    std::fs::create_dir_all(&p.bin)?;
    // In tests the running binary is the test harness: started as an
    // agent it would run the whole suite again, and again (a fork bomb).
    // Tests link a program that exits at once instead.
    #[cfg(test)]
    let exe = PathBuf::from(crate::test_stub::true_bin());
    #[cfg(not(test))]
    let exe = crate::install::real_exe()?;
    if crate::test_guard::looks_like_test_harness(&exe) {
        bail!(
            "refusing to link {} (a test harness) as the demo agents",
            exe.display()
        );
    }
    for (name, _) in Flavor::BINS {
        let file = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        let dst = p.bin.join(file);
        let _ = std::fs::remove_file(&dst);
        if std::fs::hard_link(&exe, &dst).is_err() {
            std::fs::copy(&exe, &dst)?;
        }
    }
    w(
        &p.bin.join(crate::demo::AGENT_MARKER),
        "godterm demo agent: the agent CLIs here are godterm --demo's stand-ins\n",
    )
}

fn project_files(dir: &Path, name: &str) -> Result<()> {
    w(
        &dir.join("README.md"),
        format!("# {name}\n\nDemo project for GodTerm's demo mode.\n"),
    )?;
    w(&dir.join(".gitignore"), "node_modules\ntarget\n")?;
    w(&dir.join("src").join(".keep"), "")?;
    Ok(())
}

/// Write a session's history into its transcript.
fn write_history(
    flavor: Flavor,
    config: &Path,
    cwd: &Path,
    sid: &str,
    title: &str,
    history: &[Value],
) {
    let mut t = Transcript::open(flavor, config, cwd, sid, title);
    t.title_line();
    for h in history {
        let prompt = h["prompt"].as_str().unwrap_or("");
        let summary = h["summary"].as_str().unwrap_or("");
        t.user(prompt, None);
        t.assistant(
            json!([{"type": "text", "text": summary}]),
            [6, 220 + (summary.len() as u64), 24_000, 3_100],
        );
    }
}

/// Made up Chrome profiles for the Browser button (`demo::browser_env`
/// points GodTerm here instead of the real Chrome folder).
fn chrome(home: &Path) -> Result<()> {
    let root = home.join("chrome");
    let mut cache = serde_json::Map::new();
    for (dir, name, email, on) in CHROME_PROFILES {
        cache.insert(
            dir.to_string(),
            json!({"name": name, "user_name": email, "gaia_name": ""}),
        );
        let d = root.join(dir);
        if *on {
            std::fs::create_dir_all(
                d.join("Extensions")
                    .join(crate::browser::EXTENSION_ID)
                    .join("1.0.0"),
            )?;
            w(
                &d.join("Secure Preferences"),
                json!({"extensions": {"settings": {crate::browser::EXTENSION_ID: {"state": 1}}}})
                    .to_string(),
            )?;
        } else {
            std::fs::create_dir_all(&d)?;
        }
    }
    w(
        &root.join("Local State"),
        json!({"profile": {"info_cache": cache}}).to_string(),
    )
}

/// Make the default demo world in `home` (which must be empty or a demo
/// home).
#[cfg(test)]
pub fn seed(home: &Path) -> Result<Paths> {
    seed_world(home, World::Full)
}

/// Make `world` in `home` (which must be empty or a demo home).
pub fn seed_world(home: &Path, world: World) -> Result<Paths> {
    if crate::config::is_real_user_dir(home) {
        bail!("refusing to use {} as the demo home", home.display());
    }
    let marker = home.join(".godterm-demo");
    // The web app's sign in outlives the rebuild, so a browser signed in
    // to the demo reconnects after a restart.
    let web_token = Paths::of(home).app.join("web-token");
    let kept_token = marker
        .is_file()
        .then(|| std::fs::read(&web_token).ok())
        .flatten();
    if home.exists() {
        let empty = std::fs::read_dir(home)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);
        if !empty && !marker.is_file() {
            bail!(
                "{} exists and is not a demo home (no .godterm-demo marker); pick another GODTERM_DEMO_HOME",
                home.display()
            );
        }
        // A fresh world each time: the demo always starts the same way.
        std::fs::remove_dir_all(home)?;
    }
    std::fs::create_dir_all(home)?;
    w(
        &marker,
        "made by godterm --demo; deleted and rebuilt on each start\n",
    )?;
    let p = Paths::of(home);
    link_agents(&p)?;
    for d in [&p.app, &p.work, &p.usage, &p.main_claude, &p.main_grok] {
        std::fs::create_dir_all(d)?;
    }
    if let Some(t) = kept_token {
        use crate::platform::OpenOptionsExt;
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&web_token)
        {
            let _ = f.write_all(&t);
        }
    }
    // Scenarios.
    let mut scen = scenarios();
    if world == World::Harnesses {
        // Claude keeps working next to the others (no pause after a task).
        for (name, v) in scen.iter_mut() {
            if *name == "payments-api" {
                v["pause_after"] = json!(0);
                v["repeat"] = json!(true);
                v["idle"] = json!([6.0, 10.0]);
            }
        }
    }
    for (name, s) in &scen {
        w(
            &home.join("scenarios").join(format!("{name}.json")),
            serde_json::to_string_pretty(s)?,
        )?;
        // A folder the assistant makes (pricing-page) is not there yet.
        if *name != "pricing-page" {
            project_files(&p.work.join(name), name)?;
        }
    }
    let scenario = |n: &str| scen.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone());
    chrome(home)?;
    let accts = accounts(world);
    // Accounts: config, fake logins, profiles, usage.
    let far = chrono::Utc::now().timestamp_millis() + 30 * 24 * 3600 * 1000;
    let mut toml = String::new();
    toml.push_str(&format!(
        "# GodTerm demo mode (godterm --demo). Made up accounts; rebuilt on each start.\n\
         claude_bin = {:?}\ngrok_bin = {:?}\ncodex_bin = {:?}\ncursor_bin = {:?}\n\
         antigravity_bin = {:?}\nopencode_bin = {:?}\nrefresh_secs = 2\nnotifications = false\n\
         remember_window = false\nauto_trust = true\nrestore = \"eager\"\nlayout = \"auto\"\n\
         new_tab_base = {:?}\nshow_email = false\npermission_mode = \"default\"\nsuggest_move_below = 10.0\n\n",
        agent_bin(&p, Flavor::Claude),
        agent_bin(&p, Flavor::Grok),
        agent_bin(&p, Flavor::Codex),
        agent_bin(&p, Flavor::Cursor),
        agent_bin(&p, Flavor::Antigravity),
        agent_bin(&p, Flavor::OpenCode),
        p.work.to_string_lossy(),
    ));
    let brain = if world == World::Full {
        "research"
    } else {
        "work"
    };
    toml.push_str(&format!(
        "[voice]\nenabled = false\n\n[assistant]\naccount = {brain:?}\n\n"
    ));
    for a in accts.iter().copied() {
        let dir = p.app.join("accounts").join(a.name);
        std::fs::create_dir_all(&dir)?;
        toml.push_str(&format!(
            "[[account]]\nname = {:?}\nlabel = {:?}\ncolor = {:?}\ncwd = {:?}\n{}\n",
            a.name,
            a.label,
            a.color,
            p.work.to_string_lossy(),
            if a.harness == "claude" {
                String::new()
            } else {
                format!("harness = {:?}\n", a.harness)
            }
        ));
        if !a.integrated() {
            // Codex, Cursor, Antigravity, OpenCode keep their own logins.
        } else if a.grok() {
            w(
                &dir.join("auth.json"),
                json!({"demo": true, "user": {"email": a.email}, "access_token": "demo-not-a-real-token"}).to_string(),
            )?;
        } else {
            w(
                &dir.join(".credentials.json"),
                json!({"claudeAiOauth": {"accessToken": "demo-not-a-real-token", "refreshToken": "demo-not-a-real-token",
                       "expiresAt": far, "scopes": ["user:inference"], "subscriptionType": "max", "rateLimitTier": "default_claude_max_20x"}})
                .to_string(),
            )?;
            w(
                &dir.join(".claude.json"),
                json!({"hasCompletedOnboarding": true, "oauthAccount": {"emailAddress": a.email, "organizationName": format!("{} (demo)", a.label), "billingType": "stripe_subscription"}})
                .to_string(),
            )?;
        }
        if a.integrated() {
            w(
                &p.usage.join(format!("{}.json", a.name)),
                usage_json(a, a.five, a.week),
            )?;
        }
    }
    w(&p.app.join("config.toml"), toml)?;
    w(&p.app.join("tour_done"), "")?;
    // Open tabs, with their history.
    let mut slots: Vec<Value> = accts
        .iter()
        .map(|a| json!({"account": a.name, "active": 0, "tabs": [], "groups": []}))
        .collect();
    let mut uid = 1u64;
    let now = chrono::Utc::now().timestamp() as u64;
    for t in tabs(world) {
        let ai = accts.iter().position(|a| a.name == t.acct).unwrap_or(0);
        let a = accts[ai];
        let cwd = p.work.join(t.project);
        let sc = scenario(t.project).unwrap_or_default();
        let mut tv = json!({"cwd": cwd, "uid": uid, "opened": now - 3600 * (uid % 5 + 1)});
        // GodTerm reads Claude's and Grok's transcripts (the others' tabs
        // start fresh and show their history on screen).
        if a.integrated() {
            let sid = crate::session_ops::new_uuid();
            write_history(
                a.flavor(),
                &p.app.join("accounts").join(a.name),
                &cwd,
                &sid,
                sc["title"].as_str().unwrap_or(t.project),
                sc["history"].as_array().map(Vec::as_slice).unwrap_or(&[]),
            );
            tv["session_id"] = json!(sid);
        }
        if let Some(b) = t.browser {
            tv["browser"] = json!(b);
        }
        if let Some(n) = t.name {
            tv["name"] = json!(n);
        }
        if let Some(g) = t.group {
            tv["group"] = json!(g);
            let groups = slots[ai]["groups"].as_array_mut().expect("groups");
            if !groups.iter().any(|x| x["name"] == g) {
                let color = match g {
                    "Billing" => "sand",
                    "Release" => "clay",
                    _ => "slate",
                };
                groups.push(json!({"name": g, "color": color}));
            }
        }
        if t.pinned {
            tv["pinned"] = json!(true);
        }
        if let Some(c) = t.accent {
            tv["accent"] = json!(c);
        }
        slots[ai]["tabs"].as_array_mut().expect("tabs").push(tv);
        uid += 1;
    }
    let state = json!({"focus": 0, "next_uid": uid + 1, "slots": slots});
    w(
        &p.app.join("state.json"),
        serde_json::to_string_pretty(&state)?,
    )?;
    // Past sessions (not open).
    for (acct, project, title, prompt) in past_sessions() {
        let Some(a) = accts.iter().find(|a| a.name == acct) else {
            continue;
        };
        let flavor = a.flavor();
        write_history(
            flavor,
            &p.app.join("accounts").join(acct),
            &p.work.join(project),
            &crate::session_ops::new_uuid(),
            title,
            &[task(prompt, vec![], "Done.")],
        );
    }
    // The main ~/.claude stand in: the external session's past, plus two
    // old sessions of its own.
    for (project, title, prompt) in [
        (
            "legacy-api",
            "Map the legacy API's endpoints",
            "list every endpoint the legacy API serves",
        ),
        (
            "docs-site",
            "Draft the migration guide",
            "draft a v1 to v2 migration guide",
        ),
    ] {
        write_history(
            Flavor::Claude,
            &p.main_claude,
            &p.work.join(project),
            &crate::session_ops::new_uuid(),
            title,
            &[task(prompt, vec![], "Done.")],
        );
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("godterm-demo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn files(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                files(&p, out);
            } else {
                out.push(p);
            }
        }
    }

    #[test]
    fn real_dirs_are_refused() {
        let h = crate::config::home_dir();
        for real in [
            h.join(".godterm"),
            h.join(".claude"),
            h.join(".grok"),
            h.clone(),
        ] {
            assert!(seed(&real).is_err(), "seeded {}", real.display());
            let d = crate::config::Dirs {
                app_home: real.clone(),
                main_claude: std::env::temp_dir().join("x"),
                main_grok: std::env::temp_dir().join("y"),
                explicit_home: true,
                mains: true,
            };
            assert!(std::panic::catch_unwind(|| crate::demo::assert_safe(&d)).is_err());
            let d = crate::config::Dirs {
                app_home: std::env::temp_dir().join("x"),
                main_claude: real.clone(),
                main_grok: std::env::temp_dir().join("y"),
                explicit_home: true,
                mains: true,
            };
            assert!(std::panic::catch_unwind(|| crate::demo::assert_safe(&d)).is_err());
        }
        crate::demo::assert_safe(&crate::config::Dirs {
            app_home: std::env::temp_dir().join("a"),
            main_claude: std::env::temp_dir().join("b"),
            main_grok: std::env::temp_dir().join("c"),
            explicit_home: true,
            mains: true,
        });
        // A folder of the user's that is not a demo home is never wiped.
        let d = scratch("notdemo");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("keep.txt"), "mine").unwrap();
        assert!(seed(&d).is_err());
        assert!(d.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_world_stays_in_its_home() {
        let home = scratch("world");
        let p = seed(&home).unwrap();
        // Reseeding a demo home replaces it.
        let p2 = seed(&home).unwrap();
        assert_eq!(p.app, p2.app);
        let mut all = vec![];
        files(&home, &mut all);
        assert!(all.len() > 40);
        for f in &all {
            assert!(
                f.starts_with(&home),
                "{} outside the demo home",
                f.display()
            );
        }
        // Six made up accounts: four Claude, two Grok, fake file logins.
        let cfg: toml::Value =
            toml::from_str(&std::fs::read_to_string(p.app.join("config.toml")).unwrap()).unwrap();
        let accts = cfg["account"].as_array().unwrap();
        assert_eq!(accts.len(), 6);
        assert_eq!(
            accts.iter().filter(|a| a.get("harness").is_some()).count(),
            2
        );
        assert!(cfg["claude_bin"]
            .as_str()
            .unwrap()
            .starts_with(&*home.to_string_lossy()));
        assert!(cfg["grok_bin"]
            .as_str()
            .unwrap()
            .starts_with(&*home.to_string_lossy()));
        for a in ACCOUNTS {
            let dir = p.app.join("accounts").join(a.name);
            if a.grok() {
                assert!(crate::harness::grok::logged_in(&dir));
            } else {
                // (Read as the file it is: tests never look in the keychain.)
                let c = crate::creds::parse_creds(
                    &std::fs::read_to_string(dir.join(".credentials.json")).unwrap(),
                )
                .unwrap();
                assert!(c.access_token.starts_with("demo-"));
                assert!(!c.is_expired(chrono::Utc::now().timestamp_millis()));
            }
        }
        // Sixteen open tabs, with groups and pins, each with history.
        let st: Value =
            serde_json::from_str(&std::fs::read_to_string(p.app.join("state.json")).unwrap())
                .unwrap();
        let tabs: Vec<&Value> = st["slots"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["tabs"].as_array().unwrap())
            .collect();
        assert_eq!(tabs.len(), 16);
        assert!(tabs.iter().any(|t| t["pinned"] == true));
        assert!(tabs.iter().filter(|t| t["group"].is_string()).count() >= 4);
        // Every scenario parses.
        for (name, _) in scenarios() {
            let s: crate::demo_agent::Scenario = serde_json::from_str(
                &std::fs::read_to_string(home.join("scenarios").join(format!("{name}.json")))
                    .unwrap(),
            )
            .unwrap();
            assert!(!s.title.is_empty(), "{name}");
        }
        // The agents are marked as the demo's.
        assert!(p.bin.join(crate::demo::AGENT_MARKER).is_file());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The demo's agent links are never the test binary (started as
    /// "claude" it would run the suite again, recursively). Spawns nothing.
    #[test]
    fn agent_links_are_never_the_test_binary() {
        let home = scratch("links");
        let p = Paths::of(&home);
        link_agents(&p).unwrap();
        for (n, _) in Flavor::BINS {
            let f = p.bin.join(if cfg!(windows) {
                format!("{n}.exe")
            } else {
                n.to_string()
            });
            assert!(f.is_file(), "{}", f.display());
            assert!(
                !crate::test_guard::is_current_exe(&f),
                "{n} is the test binary"
            );
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `--demo-harnesses`: one account per agent CLI, each with a tab run
    /// by the demo agent under that CLI's name, made up Chrome profiles
    /// bound to tabs, and nothing outside the demo home.
    #[test]
    fn the_harness_world_runs_every_cli() {
        let home = scratch("harnesses");
        let p = seed_world(&home, World::Harnesses).unwrap();
        let mut all = vec![];
        files(&home, &mut all);
        assert!(all.iter().all(|f| f.starts_with(&home)));
        let cfg: crate::config::Config =
            toml::from_str(&std::fs::read_to_string(p.app.join("config.toml")).unwrap()).unwrap();
        let hs: Vec<crate::harness::Harness> = cfg.accounts.iter().map(|a| a.harness()).collect();
        assert_eq!(
            hs,
            crate::harness::ALL.to_vec(),
            "one account per CLI, in order"
        );
        for &h in crate::harness::ALL {
            let bin = cfg.harness_bin(h);
            assert!(bin.starts_with(&*home.to_string_lossy()), "{bin}");
            // The link is named so the demo agent knows which CLI to be.
            let name = crate::procs::program_name(&bin);
            assert_eq!(
                Flavor::of_program(&name),
                Some(Flavor::of_harness(h)),
                "{name}"
            );
            assert!(Path::new(&bin).is_file(), "{bin}");
        }
        assert_eq!(cfg.assistant.account, "work");
        let st: Value =
            serde_json::from_str(&std::fs::read_to_string(p.app.join("state.json")).unwrap())
                .unwrap();
        let slots = st["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 6);
        for (i, s) in slots.iter().enumerate() {
            let tabs = s["tabs"].as_array().unwrap();
            assert_eq!(tabs.len(), 1, "slot {i}");
            // Only Claude and Grok tabs have a session GodTerm can read.
            assert_eq!(tabs[0]["session_id"].is_string(), i < 2, "slot {i}");
            let name = tabs[0]["cwd"]
                .as_str()
                .unwrap()
                .rsplit(['/', '\\'])
                .next()
                .unwrap()
                .to_string();
            assert!(
                home.join("scenarios")
                    .join(format!("{name}.json"))
                    .is_file(),
                "{name}"
            );
        }
        assert_eq!(slots[0]["tabs"][0]["browser"], "Default");
        // The made up Chrome profiles read as Chrome's: two with the
        // extension on, one without.
        let ext = crate::browser::chrome_extensions(&home.join("chrome"));
        assert_eq!(ext.len(), 3);
        assert_eq!(
            ext.iter()
                .filter(|e| e.installed && e.enabled == Some(true))
                .count(),
            2
        );
        assert_eq!(ext[0].profile.name, "Work");
        // No login or usage files for the CLIs that keep their own.
        for a in CLI_ACCOUNTS {
            let d = p.app.join("accounts").join(a.name);
            assert!(!d.join(".credentials.json").exists() && !d.join("auth.json").exists());
            assert!(!p.usage.join(format!("{}.json", a.name)).exists());
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn usage_fixtures_say_what_is_left() {
        let work = &ACCOUNTS[0];
        let u = crate::usage::parse_usage(&usage_json(work, 40.0, 31.0)).unwrap();
        assert_eq!(u.get("five_hour").unwrap().left().round(), 40.0);
        assert_eq!(u.get("seven_day").unwrap().left().round(), 31.0);
        let zero = crate::usage::parse_usage(&usage_json(work, 0.0, 31.0)).unwrap();
        assert_eq!(zero.get("five_hour").unwrap().left().round(), 0.0);
        let grok = ACCOUNTS.iter().find(|a| a.grok()).unwrap();
        let v: Value = serde_json::from_str(&usage_json(grok, 0.0, 74.0)).unwrap();
        let g = crate::harness::grok::parse_billing(&v);
        assert!(!g.windows.is_empty());
        assert_eq!(g.windows[0].left().round(), 74.0);
    }
}
