//! Subscription usage from `GET https://api.anthropic.com/api/oauth/usage`.

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Set to a JSON file (used for every account) or a directory holding
/// `<account name>.json` to read usage from disk instead of the API. A file
/// whose content is just `401` or `429` simulates that HTTP error.
pub const FIXTURE_ENV: &str = "GODTERM_USAGE_FIXTURE";

/// One rate limit window, e.g. five_hour or seven_day.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// The JSON key (`model_scoped:<name>` for per model buckets).
    pub key: String,
    /// Readable name, e.g. "Weekly Opus".
    pub label: String,
    /// Compact name for tight spaces, e.g. "Opus".
    pub short: String,
    /// Percent used, 0..=100 (the server may report more than 100).
    pub utilization: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

impl Window {
    /// Percent left, clamped to 0..=100.
    pub fn left(&self) -> f64 {
        (100.0 - self.utilization).clamp(0.0, 100.0)
    }

    /// Its reset time has passed: the kept numbers are from before it.
    pub fn reset_passed(&self) -> bool {
        self.resets_at.is_some_and(|t| t <= chrono::Utc::now())
    }

    /// Percent left now: full again once the reset time passed, even
    /// while the fetch that would say so has not come back.
    pub fn left_now(&self) -> f64 {
        if self.reset_passed() {
            100.0
        } else {
            self.left()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub windows: Vec<Window>,
    /// Buckets the server listed with no data (null), by readable name.
    pub absent: Vec<String>,
    /// Extra usage (pay as you go overage credits) if present.
    pub extra: Option<Extra>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Extra {
    pub enabled: bool,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub utilization: Option<f64>,
}

impl Extra {
    /// "off", "on", "on, 12.50 of 50.00 used (25%)".
    pub fn describe(&self) -> String {
        if !self.enabled {
            return "off".into();
        }
        let mut s = String::from("on");
        match (self.used, self.limit) {
            (Some(u), Some(l)) => s.push_str(&format!(", {u:.2} of {l:.2} used")),
            (Some(u), None) => s.push_str(&format!(", {u:.2} used, no limit")),
            _ => {}
        }
        if let Some(p) = self.utilization {
            s.push_str(&format!(" ({p:.0}%)"));
        }
        s
    }
}

impl Usage {
    pub fn get(&self, key: &str) -> Option<&Window> {
        self.windows.iter().find(|w| w.key == key)
    }

    /// Every window except the 5 hour and weekly headline ones.
    pub fn others(&self) -> impl Iterator<Item = &Window> {
        self.windows
            .iter()
            .filter(|w| w.key != "five_hour" && w.key != "seven_day")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum UsageError {
    NotLoggedIn,
    /// 401 or 403, or a token whose expiry has passed.
    Unauthorized,
    /// 429, with the server's Retry-After in seconds when given.
    RateLimited(Option<u64>),
    Http(u16),
    Network(String),
    Parse(String),
}

impl UsageError {
    /// Short form for the pane footer.
    pub fn short(&self) -> String {
        match self {
            UsageError::NotLoggedIn => "not logged in".into(),
            UsageError::Unauthorized => "token expired".into(),
            UsageError::RateLimited(_) => "slowed down".into(),
            UsageError::Http(c) => format!("HTTP {c}"),
            UsageError::Network(_) => "offline".into(),
            UsageError::Parse(_) => "bad response".into(),
        }
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsageError::NotLoggedIn => write!(f, "not logged in"),
            UsageError::Unauthorized => write!(
                f,
                "token expired or revoked, open this account's session to refresh it"
            ),
            UsageError::RateLimited(_) => {
                write!(f, "the usage service asked us to slow down")
            }
            UsageError::Http(c) => write!(f, "usage API returned HTTP {c}"),
            UsageError::Network(e) => write!(f, "network error: {e}"),
            UsageError::Parse(e) => write!(f, "unexpected response: {e}"),
        }
    }
}

fn title_words(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| match w {
            "oauth" => "OAuth".to_string(),
            "api" => "API".to_string(),
            _ => {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().chain(c).collect(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// (label, short) for a top level bucket key.
pub fn names_for(key: &str) -> (String, String) {
    match key {
        "five_hour" => ("5 hour".into(), "5h".into()),
        "seven_day" => ("Weekly".into(), "Week".into()),
        k => {
            if let Some(rest) = k.strip_prefix("seven_day_") {
                let r = title_words(rest);
                (format!("Weekly {r}"), r)
            } else if let Some(rest) = k.strip_prefix("five_hour_") {
                let r = title_words(rest);
                (format!("5 hour {r}"), format!("5h {r}"))
            } else {
                let t = title_words(k);
                (t.clone(), t)
            }
        }
    }
}

fn order_for(key: &str) -> usize {
    match key {
        "five_hour" => 0,
        "seven_day" => 1,
        "seven_day_opus" => 2,
        "seven_day_sonnet" => 3,
        k if k.starts_with("model_scoped") => 4,
        k if k.starts_with("seven_day") => 5,
        _ => 10,
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('%').parse().ok(),
        _ => None,
    }
}

fn parse_time(v: Option<&Value>) -> Option<DateTime<Utc>> {
    match v? {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc)),
        Value::Number(n) => {
            let t = n.as_i64()?;
            // Accept seconds or milliseconds.
            let secs = if t > 10_000_000_000 { t / 1000 } else { t };
            DateTime::from_timestamp(secs, 0)
        }
        _ => None,
    }
}

fn window_from(key: &str, (label, short): (String, String), o: &Value) -> Option<Window> {
    let utilization = num(o.get("utilization")?)?;
    Some(Window {
        key: key.to_string(),
        label,
        short,
        utilization,
        resets_at: parse_time(o.get("resets_at").or_else(|| o.get("resetsAt"))),
    })
}

/// Lenient parse that walks every key in the reply: any object with a
/// numeric `utilization` becomes a window, arrays of such objects (like
/// `model_scoped`) become one window each, `extra_usage` is understood, and
/// null buckets are remembered as absent.
pub fn parse_usage(body: &str) -> Result<Usage, UsageError> {
    let v: Value = serde_json::from_str(body).map_err(|e| UsageError::Parse(e.to_string()))?;
    let obj = v
        .as_object()
        .ok_or_else(|| UsageError::Parse("not a JSON object".into()))?;
    let mut usage = Usage::default();
    for (k, val) in obj {
        match (k.as_str(), val) {
            ("extra_usage", Value::Object(_)) => {
                usage.extra = Some(Extra {
                    enabled: val
                        .get("is_enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    used: val.get("used_credits").and_then(num),
                    limit: val.get("monthly_limit").and_then(num),
                    utilization: val.get("utilization").and_then(num),
                });
            }
            (_, Value::Object(_)) => match window_from(k, names_for(k), val) {
                Some(w) => usage.windows.push(w),
                None if val.get("utilization").is_some() => usage.absent.push(names_for(k).0),
                None => {}
            },
            // Unknown codename style null keys carry no meaning for a reader.
            (_, Value::Null) if k.starts_with("five_hour") || k.starts_with("seven_day") => {
                usage.absent.push(names_for(k).0);
            }
            (_, Value::Array(items)) => {
                for (i, it) in items.iter().enumerate() {
                    let name = it
                        .get("display_name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("{} {}", names_for(k).0, i + 1));
                    let key = format!("{k}:{name}");
                    if let Some(w) = window_from(&key, (format!("Weekly {name}"), name), it) {
                        usage.windows.push(w);
                    }
                }
            }
            _ => {}
        }
    }
    usage.windows.sort_by(|a, b| {
        order_for(&a.key)
            .cmp(&order_for(&b.key))
            .then(a.key.cmp(&b.key))
    });
    usage.absent.sort();
    Ok(usage)
}

/// Read usage from the fixture named by `GODTERM_USAGE_FIXTURE`, if set.
pub fn fixture_for(account: &str) -> Option<Result<Usage, UsageError>> {
    let _ = FIXTURE_ENV;
    let base = crate::config::env_var("USAGE_FIXTURE")?;
    let base = Path::new(&base);
    let path = if base.is_dir() {
        base.join(format!("{account}.json"))
    } else {
        base.to_path_buf()
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return Some(Err(UsageError::Network(format!("fixture: {e}")))),
    };
    Some(match text.trim() {
        "401" | "403" => Err(UsageError::Unauthorized),
        "429" => Err(UsageError::RateLimited(None)),
        body => parse_usage(body),
    })
}

/// One request: the status, Retry-After (seconds, or an HTTP date) and
/// the body, whatever the status.
pub fn fetch_usage_raw(
    url: &str,
    access_token: &str,
) -> Result<crate::usage_share::Raw, UsageError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .header("User-Agent", concat!("godterm/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| UsageError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(retry_secs);
    let body = if (200..300).contains(&status) {
        resp.body_mut()
            .read_to_string()
            .map_err(|e| UsageError::Network(e.to_string()))?
    } else {
        String::new()
    };
    Ok(crate::usage_share::Raw {
        status,
        retry_after,
        body,
    })
}

/// Retry-After as seconds: "120", or an HTTP date.
pub fn retry_secs(v: &str) -> Option<u64> {
    let v = v.trim();
    if let Ok(n) = v.parse::<u64>() {
        return Some(n);
    }
    let t = DateTime::parse_from_rfc2822(v).ok()?;
    Some((t.with_timezone(&Utc) - Utc::now()).num_seconds().max(0) as u64)
}

/// "2h 14m", "3d 4h", "now".
pub fn countdown(to: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (to - now).num_seconds();
    if secs <= 0 {
        return "now".into();
    }
    let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{}m", m.max(1))
    }
}

/// "under a minute", "3 min", "2 h", "3 days": how long ago, in words.
pub fn ago_words(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..=59 => "under a minute".into(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        s if s < 2 * 86_400 => "a day".into(),
        s => format!("{} days", s / 86_400),
    }
}

/// "4m", "2h", "3d": age of a past timestamp.
pub fn age(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/usage.json");
    const FULL: &str = include_str!("../tests/fixtures/usage_full.json");

    #[test]
    fn parses_fixture() {
        let u = parse_usage(FIXTURE).unwrap();
        let keys: Vec<_> = u.windows.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys[0], "five_hour");
        assert_eq!(keys[1], "seven_day");
        assert_eq!(keys[2], "seven_day_opus");
        assert!(keys.contains(&"model_scoped:Fable"));
        // seven_day_sonnet is null in the fixture: absent, not a window.
        assert!(!keys.contains(&"seven_day_sonnet"));
        assert!(u.absent.contains(&"Weekly Sonnet".to_string()));
        assert!(u.absent.contains(&"Weekly OAuth Apps".to_string()));
        // Codename keys with null values are ignored entirely.
        assert!(!u.absent.iter().any(|a| a.contains("Iguana")));
        let five = u.get("five_hour").unwrap();
        assert_eq!(five.utilization, 23.0);
        assert_eq!(five.left(), 77.0);
        assert_eq!(
            five.resets_at.unwrap().to_rfc3339(),
            "2099-10-06T03:00:00+00:00"
        );
        let extra = u.extra.unwrap();
        assert!(!extra.enabled);
        assert_eq!(extra.describe(), "off");
    }

    #[test]
    fn parses_every_bucket_in_full_fixture() {
        let u = parse_usage(FULL).unwrap();
        let got: Vec<(&str, &str, f64)> = u
            .windows
            .iter()
            .map(|w| (w.key.as_str(), w.label.as_str(), w.utilization))
            .collect();
        assert_eq!(
            got,
            vec![
                ("five_hour", "5 hour", 92.0),
                ("seven_day", "Weekly", 48.5),
                ("seven_day_opus", "Weekly Opus", 80.0),
                ("seven_day_sonnet", "Weekly Sonnet", 12.0),
                ("model_scoped:Fable", "Weekly Fable", 30.0),
                ("seven_day_oauth_apps", "Weekly OAuth Apps", 5.0),
                ("seven_day_overage_included", "Weekly Overage Included", 0.0),
                ("thirty_day_compute", "Thirty Day Compute", 61.0),
            ]
        );
        assert_eq!(u.get("seven_day_opus").unwrap().short, "Opus");
        assert_eq!(u.get("five_hour").unwrap().left(), 8.0);
        assert_eq!(u.others().count(), 6);
        let x = u.extra.unwrap();
        assert!(x.enabled);
        assert_eq!(x.describe(), "on, 12.50 of 50.00 used (25%)");
    }

    #[test]
    fn lenient_on_odd_shapes() {
        let u = parse_usage(
            r#"{"five_hour":{"utilization":"41.5","resets_at":1791262800},"x":3,"y":{"no":1},"seven_day":{"utilization":null}}"#,
        )
        .unwrap();
        assert_eq!(u.windows.len(), 1);
        assert_eq!(u.windows[0].utilization, 41.5);
        assert!(u.windows[0].resets_at.is_some());
        assert_eq!(u.absent, vec!["Weekly".to_string()]);
        assert!(parse_usage("[]").is_err());
        assert!(parse_usage("nope").is_err());
        assert_eq!(parse_usage("{}").unwrap(), Usage::default());
    }

    #[test]
    fn readable_names() {
        assert_eq!(names_for("seven_day_oauth_apps").0, "Weekly OAuth Apps");
        assert_eq!(names_for("five_hour_opus").1, "5h Opus");
        assert_eq!(names_for("monthly_api").0, "Monthly API");
    }

    #[test]
    fn countdown_and_age_formats() {
        let now = DateTime::parse_from_rfc3339("2026-10-06T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert_eq!(countdown(at("2026-10-06T02:14:30Z"), now), "2h 14m");
        assert_eq!(countdown(at("2026-10-09T04:00:00Z"), now), "3d 4h");
        assert_eq!(countdown(at("2026-10-06T00:00:20Z"), now), "1m");
        assert_eq!(countdown(at("2026-10-05T00:00:00Z"), now), "now");
        assert_eq!(age(at("2026-10-05T23:56:00Z"), now), "4m");
        assert_eq!(age(at("2026-10-05T21:00:00Z"), now), "3h");
    }
}
