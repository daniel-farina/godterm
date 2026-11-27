use super::{auth, http, login, remote, Hub};
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;

pub struct Ctx {
    pub peer: SocketAddr,
    pub remote: bool,
    pub gate: bool,
    pub sid: Option<String>,
    pub authed: bool,
}

const PUBLIC_BODY: usize = 8 << 10;

fn err(s: &mut TcpStream, status: &str, msg: &str) -> std::io::Result<()> {
    http::json(s, status, &json!({"ok": false, "error": msg}), &[])
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn limiter_key(req: &http::Request, peer: IpAddr) -> String {
    req.header("x-forwarded-for")
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| v.parse::<IpAddr>().is_ok())
        .unwrap_or_else(|| peer.to_string())
}

impl Hub {
    fn read_json(
        &self,
        s: &mut TcpStream,
        req: &http::Request,
        max: usize,
    ) -> Result<Value, std::io::Result<()>> {
        let json_type = req
            .header("content-type")
            .is_some_and(|c| c.to_ascii_lowercase().starts_with("application/json"));
        if !json_type {
            return Err(err(s, "415 Unsupported Media Type", "send JSON"));
        }
        let Some(b) = http::read_body(s, req, max) else {
            return Err(err(s, "400 Bad Request", "bad request body"));
        };
        serde_json::from_slice::<Value>(&b)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| err(s, "400 Bad Request", "bad JSON"))
    }

    fn session_cookie(&self, value: &str) -> (&'static str, String) {
        let secure = self
            .login
            .remote()
            .is_some_and(|(o, _)| o.starts_with("https://"));
        ("Set-Cookie", login::set_session_cookie(value, secure))
    }

    fn origin_is_remote(&self, req: &http::Request) -> bool {
        let Some((origin, _)) = self.login.remote() else {
            return false;
        };
        req.header("origin")
            .is_some_and(|o| o.trim_end_matches('/').eq_ignore_ascii_case(&origin))
    }

    fn device(&self, req: &http::Request) -> String {
        login::device_label(req.header("user-agent").unwrap_or(""))
    }

    pub(super) fn auth_api(
        self: &Arc<Self>,
        s: &mut TcpStream,
        req: &http::Request,
        cx: &Ctx,
    ) -> std::io::Result<()> {
        http::set_keep(false);
        let name = req.path.trim_start_matches("/auth/");
        let host = req.header("host").unwrap_or("").to_string();
        let now = login::now();
        let get = req.method == "GET";
        let post = req.method == "POST";
        if !get && !post {
            return http::text(s, "405 Method Not Allowed", "GET or POST only");
        }

        match (get, name) {
            (true, "state") => {
                let on = self.login.enabled();
                return http::json(
                    s,
                    "200 OK",
                    &json!({"enabled": on, "origin": self.login.remote().map(|r| r.0).unwrap_or_default()}),
                    &[],
                );
            }
            (true, "pair") => return self.pair_in(s, req, cx, now),
            (false, "login/begin") => {
                if let Err(r) = self.read_json(s, req, PUBLIC_BODY) {
                    return r;
                }
                if !self.origin_is_remote(req) {
                    return err(s, "403 Forbidden", "bad origin");
                }
                return match self.login.login_begin(now) {
                    Ok(v) => http::json(s, "200 OK", &v, &[]),
                    Err(e) => err(s, "400 Bad Request", &e),
                };
            }
            (false, "login/finish") | (false, "recovery/use") => {
                let v = match self.read_json(s, req, PUBLIC_BODY) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                if !self.origin_is_remote(req) {
                    return err(s, "403 Forbidden", "bad origin");
                }
                let key = limiter_key(req, cx.peer.ip());
                if self.login.blocked(&key) {
                    crate::log::info("web: sign in refused: too many failures, waiting");
                    return err(s, "429 Too Many Requests", "too many tries; wait a minute");
                }
                let label = self.device(req);
                let cookie = if name == "login/finish" {
                    self.login
                        .login_finish(
                            str_of(&v, "cid"),
                            str_of(&v, "id"),
                            str_of(&v, "clientDataJSON"),
                            str_of(&v, "authenticatorData"),
                            str_of(&v, "signature"),
                            &label,
                            now,
                        )
                        .ok()
                } else {
                    self.login.use_recovery(str_of(&v, "code"), &label, now)
                };
                return match cookie {
                    Some(c) => {
                        crate::log::info("web: a browser signed in");
                        let h = self.session_cookie(&c);
                        http::json(s, "200 OK", &json!({"ok": true}), &[h])
                    }
                    None => {
                        self.login.fail(&key);
                        crate::log::info("web: a sign in was refused");
                        err(s, "401 Unauthorized", "sign in failed")
                    }
                };
            }
            _ => {}
        }

        if post && !auth::origin_ok(req.header("origin"), &host, req.header("x-forwarded-host")) {
            return err(s, "403 Forbidden", "bad origin");
        }
        if auth::from_inside(cx.peer, self.local_peer(cx.peer.ip())) {
            crate::log::info("web: a login setting was refused from a process GodTerm started");
            return err(
                s,
                "403 Forbidden",
                "not from inside GodTerm: its tabs cannot change login",
            );
        }
        if !cx.authed {
            return err(s, "401 Unauthorized", "sign in first");
        }
        let body = if post {
            match self.read_json(s, req, http::MAX_BODY) {
                Ok(v) => v,
                Err(r) => return r,
            }
        } else {
            json!({})
        };

        match (get, name) {
            (true, "status") => {
                let cur = cx.sid.clone();
                let sessions: Vec<Value> = self
                    .login
                    .sessions(now)
                    .into_iter()
                    .map(|mut v| {
                        v["current"] =
                            json!(Some(v["sid"].as_str().unwrap_or("")) == cur.as_deref());
                        v
                    })
                    .collect();
                http::json(
                    s,
                    "200 OK",
                    &json!({
                        "ok": true,
                        "login": self.login.status(now),
                        "passkeys": self.login.passkeys(),
                        "sessions": sessions,
                        "here": {"remote": cx.remote, "gate": cx.gate, "session": cx.sid.is_some()},
                        "web_port": self.port,
                    }),
                    &[],
                )
            }
            (true, "remote") => {
                let run = self
                    .ts_run
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let r = remote::status(&*run, self.port);
                if !r.url.is_empty() && self.login.remote().is_none() {
                    let _ = self.login.set_remote_url(&r.url);
                }
                http::json(s, "200 OK", &json!({"ok": true, "remote": r.json()}), &[])
            }
            (false, "remote") => self.remote_action(s, &body),
            (false, "register/begin") => match self.login.register_begin(now) {
                Ok(v) => http::json(s, "200 OK", &v, &[]),
                Err(e) => err(s, "400 Bad Request", &e),
            },
            (false, "register/finish") => {
                let label = {
                    let l = str_of(&body, "label");
                    if l.is_empty() {
                        self.device(req)
                    } else {
                        l.to_string()
                    }
                };
                match self.login.register_finish(
                    str_of(&body, "cid"),
                    &label,
                    str_of(&body, "clientDataJSON"),
                    str_of(&body, "attestationObject"),
                    now,
                ) {
                    Ok(p) => http::json(s, "200 OK", &json!({"ok": true, "passkey": p}), &[]),
                    Err(e) => err(s, "400 Bad Request", &e),
                }
            }
            (false, "passkey/rename") => {
                if self
                    .login
                    .rename_passkey(str_of(&body, "pid"), str_of(&body, "label"))
                {
                    http::json(s, "200 OK", &json!({"ok": true}), &[])
                } else {
                    err(s, "400 Bad Request", "could not rename")
                }
            }
            (false, "passkey/remove") => match self.login.remove_passkey(str_of(&body, "pid")) {
                Ok(()) => http::json(s, "200 OK", &json!({"ok": true}), &[]),
                Err(e) => err(s, "400 Bad Request", &e),
            },
            (false, "enable") => match self.login.enable() {
                Ok(code) => {
                    crate::log::info("web: login was turned on");
                    let mut extra = vec![];
                    if cx.remote && cx.sid.is_none() {
                        let v = self.login.issue_session(&self.device(req), now);
                        extra.push(self.session_cookie(&v));
                    }
                    self.kick_stale();
                    http::json(s, "200 OK", &json!({"ok": true, "recovery": code}), &extra)
                }
                Err(e) => err(s, "400 Bad Request", &e),
            },
            (false, "disable") => {
                let run = self
                    .ts_run
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Err(e) = remote::make_private(&*run, self.port) {
                    return err(
                        s,
                        "502 Bad Gateway",
                        &format!("could not close Funnel first: {e}"),
                    );
                }
                self.login.disable(now);
                crate::log::info("web: login was turned off");
                self.kick_stale();
                let secure = self
                    .login
                    .remote()
                    .is_some_and(|r| r.0.starts_with("https://"));
                http::json(
                    s,
                    "200 OK",
                    &json!({"ok": true}),
                    &[("Set-Cookie", login::clear_session_cookie(secure))],
                )
            }
            (false, "recovery/new") => {
                if !self.login.enabled() {
                    return err(s, "400 Bad Request", "login is off");
                }
                let code = self.login.new_recovery();
                http::json(s, "200 OK", &json!({"ok": true, "recovery": code}), &[])
            }
            (false, "pair") => {
                if self.login.remote().is_none() {
                    let run = self
                        .ts_run
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let r = remote::status(&*run, self.port);
                    if !r.url.is_empty() {
                        let _ = self.login.set_remote_url(&r.url);
                    }
                }
                let Some((origin, _)) = self.login.remote() else {
                    return err(s, "400 Bad Request", "create the remote address first");
                };
                let code = self.login.new_pair(now);
                http::json(
                    s,
                    "200 OK",
                    &json!({"ok": true, "url": format!("{origin}/auth/pair?c={code}"), "valid_s": login::PAIR_TTL}),
                    &[],
                )
            }
            (false, "session/revoke") => {
                let sid = str_of(&body, "sid");
                let gone = self.login.revoke_session(sid, now);
                self.kick_stale();
                let own = cx.sid.as_deref() == Some(sid);
                let secure = self
                    .login
                    .remote()
                    .is_some_and(|r| r.0.starts_with("https://"));
                let extra = if own {
                    vec![("Set-Cookie", login::clear_session_cookie(secure))]
                } else {
                    vec![]
                };
                http::json(s, "200 OK", &json!({"ok": gone}), &extra)
            }
            (false, "session/revoke_all") | (false, "logout") => {
                let keep = if name == "logout" {
                    None
                } else {
                    body["keep_this"]
                        .as_bool()
                        .unwrap_or(false)
                        .then(|| cx.sid.clone())
                        .flatten()
                };
                let n = if name == "logout" {
                    cx.sid
                        .as_deref()
                        .map(|s| self.login.revoke_session(s, now) as usize)
                        .unwrap_or(0)
                } else {
                    self.login.revoke_all(keep.as_deref(), now)
                };
                self.kick_stale();
                let secure = self
                    .login
                    .remote()
                    .is_some_and(|r| r.0.starts_with("https://"));
                let extra = if keep.is_none() {
                    vec![("Set-Cookie", login::clear_session_cookie(secure))]
                } else {
                    vec![]
                };
                http::json(s, "200 OK", &json!({"ok": true, "signed_out": n}), &extra)
            }
            _ => err(s, "404 Not Found", "not found"),
        }
    }

    fn pair_in(
        &self,
        s: &mut TcpStream,
        req: &http::Request,
        cx: &Ctx,
        now: i64,
    ) -> std::io::Result<()> {
        let Some((origin, rp)) = self.login.remote() else {
            return http::text(s, "404 Not Found", "not found");
        };
        let host = req
            .header("x-forwarded-host")
            .or_else(|| req.header("host"))
            .unwrap_or("");
        if auth::host_name(host) != rp {
            return http::text(s, "400 Bad Request", "open this link on the remote address");
        }
        let key = limiter_key(req, cx.peer.ip());
        if self.login.blocked(&key) {
            return http::text(s, "429 Too Many Requests", "too many tries; wait a minute");
        }
        if auth::from_inside(cx.peer, self.local_peer(cx.peer.ip())) {
            return http::text(s, "403 Forbidden", "not from inside GodTerm");
        }
        let code = req.param("c").unwrap_or("");
        if code.len() != 48 || !self.login.use_pair(code, now) {
            self.login.fail(&key);
            crate::log::info("web: a pairing link was refused");
            return http::text(s, "403 Forbidden", "this link is used up or has expired");
        }
        crate::log::info("web: a browser was paired");
        let v = self.login.issue_session(&self.device(req), now);
        let secure = origin.starts_with("https://");
        http::respond(
            s,
            "303 See Other",
            &[
                ("Location", "/".into()),
                ("Cache-Control", "no-store".into()),
                ("Set-Cookie", login::set_session_cookie(&v, secure)),
            ],
            &[],
            false,
        )
    }

    fn remote_action(&self, s: &mut TcpStream, body: &Value) -> std::io::Result<()> {
        let run = self
            .ts_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let run = &*run;
        let fail = |s: &mut TcpStream, e: &str| err(s, "400 Bad Request", e);
        let result = match str_of(body, "action") {
            "serve" => remote::create(run, self.port, false)
                .and_then(|r| self.login.set_remote_url(&r.url).map(|_| r)),
            "funnel" => {
                if !self.login.enabled() {
                    return fail(
                        s,
                        "Turn login on first: Funnel makes the address reachable from the whole internet",
                    );
                }
                remote::create(run, self.port, true)
                    .and_then(|r| self.login.set_remote_url(&r.url).map(|_| r))
            }
            "private" => remote::make_private(run, self.port),
            "off" => {
                if self.login.enabled() {
                    return fail(s, "Turn login off before removing the address");
                }
                match remote::remove(run, self.port).and_then(|_| self.login.clear_remote_url()) {
                    Ok(()) => Ok(remote::status(run, self.port)),
                    Err(e) => Err(e),
                }
            }
            _ => return fail(s, "unknown action"),
        };
        match result {
            Ok(r) => http::json(s, "200 OK", &json!({"ok": true, "remote": r.json()}), &[]),
            Err(e) => fail(s, &e),
        }
    }
}
