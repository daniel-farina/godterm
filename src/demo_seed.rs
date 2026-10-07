//! The demo's made up world: six accounts (four Claude, two Grok) with
//! fake logins and usage, sixteen sessions with scenarios and history,
//! past sessions to search, and one "external" session in another
//! terminal. Written into the demo home by `godterm --demo`.

use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::demo_agent::{Flavor, Transcript};

/// One demo account.
pub struct Acct {
    pub name: &'static str,
    pub label: &'static str,
    pub grok: bool,
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
        grok: false,
        color: "sand",
        email: "alex@acme-corp.dev",
        five: 40.0,
        week: 31.0,
    },
    Acct {
        name: "personal",
        label: "Personal",
        grok: false,
        color: "sage",
        email: "alex.rivera@mailbox.dev",
        five: 63.0,
        week: 71.0,
    },
    Acct {
        name: "research",
        label: "Research",
        grok: false,
        color: "slate",
        email: "alex@lab.example.org",
        five: 88.0,
        week: 79.0,
    },
    Acct {
        name: "client",
        label: "Client",
        grok: false,
        color: "clay",
        email: "alex@northwind-studio.dev",
        five: 47.0,
        week: 36.0,
    },
    Acct {
        name: "grok-lab",
        label: "Grok Lab",
        grok: true,
        color: "mauve",
        email: "alex@grok-lab.dev",
        five: 0.0,
        week: 74.0,
    },
    Acct {
        name: "grok-ops",
        label: "Grok Ops",
        grok: true,
        color: "stone",
        email: "ops@acme-corp.dev",
        five: 0.0,
        week: 42.0,
    },
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
}

const fn tab(acct: &'static str, project: &'static str) -> Tab {
    Tab {
        acct,
        project,
        name: None,
        group: None,
        pinned: false,
        accent: None,
    }
}

fn tabs() -> Vec<Tab> {
    vec![
        Tab {
            pinned: true,
            group: Some("Billing"),
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
    if a.grok {
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

/// Link (or copy) the running godterm as `bin/claude` and `bin/grok`.
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
    for name in ["claude", "grok"] {
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
        "godterm demo agent: claude and grok here are godterm --demo's stand-ins\n",
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

/// Make the demo world in `home` (which must be empty or a demo home).
pub fn seed(home: &Path) -> Result<Paths> {
    if crate::config::is_real_user_dir(home) {
        bail!("refusing to use {} as the demo home", home.display());
    }
    let marker = home.join(".godterm-demo");
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
    // Scenarios.
    let scen = scenarios();
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
    // Accounts: config, fake logins, profiles, usage.
    let far = chrono::Utc::now().timestamp_millis() + 30 * 24 * 3600 * 1000;
    let mut toml = String::new();
    toml.push_str(&format!(
        "# GodTerm demo mode (godterm --demo). Made up accounts; rebuilt on each start.\n\
         claude_bin = {:?}\ngrok_bin = {:?}\nrefresh_secs = 2\nnotifications = false\n\
         remember_window = false\nauto_trust = true\nrestore = \"eager\"\nlayout = \"auto\"\n\
         new_tab_base = {:?}\nshow_email = false\npermission_mode = \"default\"\nsuggest_move_below = 10.0\n\n",
        p.bin.join("claude").to_string_lossy(),
        p.bin.join("grok").to_string_lossy(),
        p.work.to_string_lossy(),
    ));
    toml.push_str("[voice]\nenabled = false\n\n[assistant]\naccount = \"research\"\n\n");
    for a in ACCOUNTS {
        let dir = p.app.join("accounts").join(a.name);
        std::fs::create_dir_all(&dir)?;
        toml.push_str(&format!(
            "[[account]]\nname = {:?}\nlabel = {:?}\ncolor = {:?}\ncwd = {:?}\n{}\n",
            a.name,
            a.label,
            a.color,
            p.work.to_string_lossy(),
            if a.grok { "harness = \"grok\"\n" } else { "" }
        ));
        if a.grok {
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
        w(
            &p.usage.join(format!("{}.json", a.name)),
            usage_json(a, a.five, a.week),
        )?;
    }
    w(&p.app.join("config.toml"), toml)?;
    w(&p.app.join("tour_done"), "")?;
    // Open tabs, with their history.
    let mut slots: Vec<Value> = ACCOUNTS
        .iter()
        .map(|a| json!({"account": a.name, "active": 0, "tabs": [], "groups": []}))
        .collect();
    let mut uid = 1u64;
    let now = chrono::Utc::now().timestamp() as u64;
    for t in tabs() {
        let ai = ACCOUNTS.iter().position(|a| a.name == t.acct).unwrap_or(0);
        let a = &ACCOUNTS[ai];
        let flavor = if a.grok { Flavor::Grok } else { Flavor::Claude };
        let cwd = p.work.join(t.project);
        let sid = crate::session_ops::new_uuid();
        let sc = scenario(t.project).unwrap_or_default();
        let config = p.app.join("accounts").join(a.name);
        write_history(
            flavor,
            &config,
            &cwd,
            &sid,
            sc["title"].as_str().unwrap_or(t.project),
            sc["history"].as_array().map(Vec::as_slice).unwrap_or(&[]),
        );
        let mut tv = json!({"cwd": cwd, "session_id": sid, "uid": uid, "opened": now - 3600 * (uid % 5 + 1)});
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
        let a = ACCOUNTS.iter().find(|a| a.name == acct).expect("account");
        let flavor = if a.grok { Flavor::Grok } else { Flavor::Claude };
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
            if a.grok {
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
        for n in ["claude", "grok"] {
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

    #[test]
    fn usage_fixtures_say_what_is_left() {
        let work = &ACCOUNTS[0];
        let u = crate::usage::parse_usage(&usage_json(work, 40.0, 31.0)).unwrap();
        assert_eq!(u.get("five_hour").unwrap().left().round(), 40.0);
        assert_eq!(u.get("seven_day").unwrap().left().round(), 31.0);
        let zero = crate::usage::parse_usage(&usage_json(work, 0.0, 31.0)).unwrap();
        assert_eq!(zero.get("five_hour").unwrap().left().round(), 0.0);
        let grok = ACCOUNTS.iter().find(|a| a.grok).unwrap();
        let v: Value = serde_json::from_str(&usage_json(grok, 0.0, 74.0)).unwrap();
        let g = crate::harness::grok::parse_billing(&v);
        assert!(!g.windows.is_empty());
        assert_eq!(g.windows[0].left().round(), 74.0);
    }
}
