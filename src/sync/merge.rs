use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub const TOMBSTONE_MS: i64 = 90 * 24 * 3600 * 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub updated: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,

    #[serde(default)]
    pub value: Value,
}

pub type Doc = BTreeMap<String, Entry>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaseEnt {
    pub h: String,
    pub u: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub d: bool,
}

pub type Base = BTreeMap<String, BaseEnt>;

pub fn hash(v: &Value) -> String {
    let h = super::sha_hex(v.to_string().as_bytes());
    h[..16].to_string()
}

pub fn stamp(local: &BTreeMap<String, Value>, base: &Base, now: i64) -> Doc {
    let mut d = Doc::new();
    for (k, v) in local {
        let h = hash(v);
        let updated = match base.get(k) {
            Some(b) if b.h == h && !b.d => b.u,
            Some(b) => now.max(b.u + 1),
            None => now,
        };
        d.insert(
            k.clone(),
            Entry {
                updated,
                deleted: false,
                value: v.clone(),
            },
        );
    }
    for (k, b) in base {
        if local.contains_key(k) {
            continue;
        }
        let updated = if b.d { b.u } else { now.max(b.u + 1) };
        d.insert(
            k.clone(),
            Entry {
                updated,
                deleted: true,
                value: Value::Null,
            },
        );
    }
    d
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    Same,
    Local,
    Remote,

    Conflict { local_won: bool },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Merged {
    pub doc: Doc,
    pub picks: BTreeMap<String, Pick>,
}

impl Merged {
    pub fn to_home(&self) -> Vec<&String> {
        self.picks
            .iter()
            .filter(|(_, p)| matches!(p, Pick::Remote | Pick::Conflict { local_won: false }))
            .map(|(k, _)| k)
            .collect()
    }

    pub fn live(&self) -> BTreeMap<String, Value> {
        self.doc
            .iter()
            .filter(|(_, e)| !e.deleted)
            .map(|(k, e)| (k.clone(), e.value.clone()))
            .collect()
    }

    pub fn base(&self) -> Base {
        self.doc
            .iter()
            .map(|(k, e)| {
                (
                    k.clone(),
                    BaseEnt {
                        h: if e.deleted {
                            String::new()
                        } else {
                            hash(&e.value)
                        },
                        u: e.updated,
                        d: e.deleted,
                    },
                )
            })
            .collect()
    }
}

fn changed(e: &Entry, b: Option<&BaseEnt>) -> bool {
    match b {
        None => true,
        Some(b) => b.d != e.deleted || (!e.deleted && b.h != hash(&e.value)),
    }
}

fn newer(a: &Entry, b: &Entry) -> bool {
    (a.updated, hash(&a.value)) > (b.updated, hash(&b.value))
}

pub fn merge(local: &Doc, remote: &Doc, base: &Base, now: i64) -> Merged {
    let mut m = Merged::default();
    let keys: std::collections::BTreeSet<&String> = local.keys().chain(remote.keys()).collect();
    for k in keys {
        let (e, pick) = match (local.get(k), remote.get(k)) {
            (Some(l), None) => (l.clone(), Pick::Local),
            (None, Some(r)) => (r.clone(), Pick::Remote),
            (Some(l), Some(r)) => {
                if l.deleted == r.deleted && (l.deleted || l.value == r.value) {
                    let mut e = l.clone();
                    e.updated = l.updated.max(r.updated);
                    (e, Pick::Same)
                } else {
                    let b = base.get(k);
                    match (changed(l, b), changed(r, b)) {
                        (false, _) => (r.clone(), Pick::Remote),
                        (true, false) => (l.clone(), Pick::Local),
                        (true, true) => {
                            if newer(l, r) {
                                (l.clone(), Pick::Conflict { local_won: true })
                            } else {
                                (r.clone(), Pick::Conflict { local_won: false })
                            }
                        }
                    }
                }
            }
            (None, None) => continue,
        };
        if e.deleted && now - e.updated > TOMBSTONE_MS {
            continue;
        }
        m.picks.insert(k.clone(), pick);
        m.doc.insert(k.clone(), e);
    }
    m
}

pub fn load_base(p: &Path) -> Base {
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_base(p: &Path, b: &Base) -> std::io::Result<()> {
    super::write_file(p, &serde_json::to_vec(b).unwrap_or_default())
}

pub fn load_doc(p: &Path) -> Doc {
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice::<DocFile>(&b).ok())
        .map(|f| f.entries)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: Doc,
}

pub fn doc_bytes(d: &Doc) -> Vec<u8> {
    let mut v = serde_json::to_vec_pretty(&DocFile {
        version: 1,
        entries: d.clone(),
    })
    .unwrap_or_default();
    v.push(b'\n');
    v
}

#[derive(Debug, Clone, Copy)]
pub struct Store {
    pub name: &'static str,

    pub file: &'static str,

    pub list: &'static str,
    pub key: fn(&Value) -> Option<String>,
}

fn by_id(v: &Value) -> Option<String> {
    match &v["id"] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn by_created(v: &Value) -> Option<String> {
    match v["created"].as_str() {
        Some(c) if !c.is_empty() => Some(c.to_string()),
        _ => by_id(v),
    }
}

fn by_memo(v: &Value) -> Option<String> {
    Some(format!("{}|{}", v["kind"].as_str()?, v["key"].as_str()?))
}

pub const STORES: &[Store] = &[
    Store {
        name: "archive",
        file: "archive.json",
        list: "entries",
        key: by_id,
    },
    Store {
        name: "jobs",
        file: "jobs.json",
        list: "jobs",
        key: by_id,
    },
    Store {
        name: "projects",
        file: "projects.json",
        list: "items",
        key: by_memo,
    },
    Store {
        name: "learned",
        file: "assistant/learned.json",
        list: "rules",
        key: by_created,
    },
];

pub fn entries_of(
    s: &Store,
    whole: &Value,
    fix: &dyn Fn(&str, &mut Value),
) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for e in whole[s.list].as_array().into_iter().flatten() {
        let mut e = e.clone();
        fix(s.name, &mut e);
        if let Some(k) = (s.key)(&e) {
            out.insert(k, e);
        }
    }

    if s.name == "projects" {
        if let Some(o) = whole["deep_searched"].as_object() {
            for (k, v) in o {
                let mut x = serde_json::json!({"kind": "deep_searched", "key": k, "day": v});
                fix(s.name, &mut x);
                if let Some(k) = by_memo(&x) {
                    out.insert(k, x);
                }
            }
        }
    }
    out
}

pub fn with_entries(
    s: &Store,
    whole: &Value,
    live: &BTreeMap<String, Value>,
    fix: &dyn Fn(&str, &mut Value),
) -> Value {
    let mut out = if whole.is_object() {
        whole.clone()
    } else {
        serde_json::json!({})
    };
    let mut list = vec![];
    let mut ds = serde_json::Map::new();
    for v in live.values() {
        let mut v = v.clone();
        fix(s.name, &mut v);
        if s.name == "projects" && v["kind"] == "deep_searched" {
            if let Some(k) = v["key"].as_str() {
                ds.insert(k.to_string(), v["day"].clone());
            }
            continue;
        }
        list.push(v);
    }

    let order: Vec<String> = whole[s.list]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| (s.key)(e))
        .collect();
    list.sort_by_key(|e| {
        let k = (s.key)(e).unwrap_or_default();
        (order.iter().position(|o| *o == k).unwrap_or(usize::MAX), k)
    });
    if s.name == "learned" {
        renumber_rules(&mut list);
        let max = list
            .iter()
            .filter_map(|r| r["id"].as_u64())
            .max()
            .unwrap_or(0);
        let next = out["next_id"].as_u64().unwrap_or(0).max(max + 1);
        out["next_id"] = serde_json::json!(next);
        if out["rev"].is_null() {
            out["rev"] = serde_json::json!(0);
        }
    }
    out[s.list] = Value::Array(list);
    if s.name == "projects" {
        out["deep_searched"] = Value::Object(ds);
        if out["version"].is_null() {
            out["version"] = serde_json::json!(1);
        }
    }
    out
}

fn renumber_rules(list: &mut [Value]) {
    let mut idx: Vec<usize> = (0..list.len()).collect();
    idx.sort_by_key(|&i| list[i]["created"].as_str().unwrap_or("").to_string());
    let mut seen = std::collections::BTreeSet::new();
    let mut max = list
        .iter()
        .filter_map(|r| r["id"].as_u64())
        .max()
        .unwrap_or(0);
    for i in idx {
        let id = list[i]["id"].as_u64().unwrap_or(0);
        if !seen.insert(id) {
            max += 1;
            list[i]["id"] = serde_json::json!(max);
            seen.insert(max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vals(v: &[(&str, Value)]) -> BTreeMap<String, Value> {
        v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect()
    }

    #[test]
    fn keyed_merge_with_tombstones_and_skew() {
        let day = 24 * 3600 * 1000;
        let t0 = 1_800_000_000_000;

        let synced = vals(&[
            ("a", json!({"n": 1})),
            ("b", json!({"n": 2})),
            ("c", json!({"n": 3})),
        ]);
        let base0 = merge(
            &stamp(&synced, &Base::new(), t0),
            &Doc::new(),
            &Base::new(),
            t0,
        )
        .base();

        let a_local = vals(&[
            ("a", json!({"n": 10})),
            ("c", json!({"n": 3})),
            ("d", json!({"n": 4})),
        ]);
        let a = stamp(&a_local, &base0, t0 + 10_000);
        assert!(a["b"].deleted && a["b"].updated == t0 + 10_000);
        assert_eq!(a["c"].updated, t0);

        let b_local = vals(&[
            ("a", json!({"n": 20})),
            ("b", json!({"n": 2})),
            ("c", json!({"n": 30})),
        ]);
        let b = stamp(&b_local, &base0, t0 - 3_600_000);

        assert_eq!(b["c"].updated, t0 + 1);

        let m = merge(&b, &a, &base0, t0);
        let live = m.live();
        assert_eq!(live["c"], json!({"n": 30}), "only B changed c");
        assert!(!live.contains_key("b"), "A deleted b, B did not touch it");
        assert_eq!(live["d"], json!({"n": 4}));

        assert_eq!(live["a"], json!({"n": 10}));
        assert_eq!(m.picks["a"], Pick::Conflict { local_won: false });
        assert_eq!(m.picks["c"], Pick::Local);

        let m2 = merge(&a, &m.doc, &base0, t0 + 20_000);
        assert_eq!(m2.live(), live);

        let base1 = m.base();
        let again = stamp(&vals(&[("b", json!({"n": 5}))]), &base1, t0 + 30_000);
        assert!(!again["b"].deleted && again["b"].updated > m.doc["b"].updated);

        let late = merge(&m.doc, &Doc::new(), &base1, t0 + 91 * day);
        assert!(!late.doc.contains_key("b"));
        assert!(late.doc.contains_key("a"));

        let s = merge(&a, &a, &base0, t0);
        assert!(s.to_home().is_empty());
    }

    #[test]
    fn stores_round_trip_and_rules_renumber() {
        let s = STORES.iter().find(|s| s.name == "learned").unwrap();
        let whole = json!({"rules": [
            {"id": 1, "text": "x", "created": "2026-01-01T00:00:00Z"},
            {"id": 2, "text": "mine", "created": "2026-02-01T00:00:00Z"}
        ], "next_id": 3, "rev": 4});
        let nofix = |_: &str, _: &mut Value| {};
        let mut e = entries_of(s, &whole, &nofix);

        e.insert(
            "2026-03-01T00:00:00Z".into(),
            json!({"id": 2, "text": "theirs", "created": "2026-03-01T00:00:00Z"}),
        );
        let out = with_entries(s, &whole, &e, &nofix);
        let ids: Vec<u64> = out["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(out["rules"][2]["text"], "theirs");
        assert_eq!(out["next_id"], 4);
        assert_eq!(out["rev"], 4);
        let p = STORES.iter().find(|s| s.name == "projects").unwrap();
        let whole = json!({"version": 1, "items": [{"kind": "project", "key": "/Users/alice/x"}], "deep_searched": {"/Users/alice/y": "2026-10-01"}});
        let e = entries_of(p, &whole, &nofix);
        assert_eq!(e.len(), 2);
        let back = with_entries(p, &whole, &e, &nofix);
        assert_eq!(back, whole);
    }
}
