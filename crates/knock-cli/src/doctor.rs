//! `knock doctor` (SPEC.md FR-12, §13.3): the app's three health checks from
//! the command line, one plain-language line each, marked PASS or FAIL.
//!
//! 1. Connected to Claude Code: Knock's §8.2 entries in the Claude Code
//!    settings file, for this port and token (Installed / Stale / Missing, as
//!    the app's `hooks::Setup::status`).
//! 2. Listening for Claude Code: `GET /v1/health` on 127.0.0.1 with the token
//!    answers `200` (§8.1).
//! 3. Last event from Claude Code: `lastEventAt` from that reply.
//!
//! Strictly read-only (§8.2, §11.6): it reads `%APPDATA%\Knock\server.json`
//! and the settings file, and never writes, creates, renames or backs up
//! anything. Its output holds no token, no request or reply body and no path
//! beyond a file name (§11.5).
//!
//! The settings rules are duplicated from the app's `hooks.rs`, because the
//! CLI can't depend on the app crate; each copy names its original.

use knock_core::{Adapter, ClaudeCode, HookSpec};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value, json};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// §8.2 ownership: Knock's entries' URLs start with this (hooks.rs:28).
const OWNER: &str = "http://127.0.0.1:";
const TOKEN_HEADER: &str = "X-Knock-Token";
const BOM: &[u8] = b"\xEF\xBB\xBF";
/// §8.1 port range (server.rs:29).
const PORTS: RangeInclusive<u16> = 49152..=65535;
/// Windows retries a refused loopback connect for about 2 s (see `sim.rs`).
const TIMEOUT: Duration = Duration::from_secs(2);
/// A health reply is under 200 bytes; read no more than this.
const REPLY_LIMIT: u64 = 64 * 1024;

const HOOKS: &str = "Connected to Claude Code";
const SERVER: &str = "Listening for Claude Code";
const LAST: &str = "Last event from Claude Code";

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub label: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn check(label: &'static str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        label,
        ok,
        detail: detail.into(),
    }
}

/// The three checks, in the app's order, and an optional "Knock <version> is
/// running" line.
#[derive(Debug)]
pub struct Report {
    pub checks: Vec<Check>,
    pub info: Option<String>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for c in &self.checks {
            let mark = if c.ok { "PASS" } else { "FAIL" };
            out += &format!("{mark}  {}: {}\n", c.label, c.detail);
        }
        if let Some(info) = &self.info {
            out += &format!("{info}\n");
        }
        out
    }
}

/// Where doctor reads from. Tests point these at temporary files.
#[derive(Debug, Clone, Default)]
pub struct Paths {
    /// The Claude Code settings file; `None` if neither variable is set.
    pub settings: Option<PathBuf>,
    /// `%APPDATA%\Knock\server.json`; `None` if `APPDATA` isn't set.
    pub server: Option<PathBuf>,
}

impl Paths {
    pub fn from_env(env: impl Fn(&str) -> Option<OsString>) -> Self {
        let server = (env("APPDATA").filter(|v| !v.is_empty()))
            .map(|d| PathBuf::from(d).join("Knock").join("server.json"));
        Self {
            settings: claude_settings(&env),
            server,
        }
    }
}

/// The file Claude Code reads (§8.2, ADR 0022): `<CLAUDE_CONFIG_DIR>\
/// settings.json` if that variable is non-empty, else `%USERPROFILE%\.claude\
/// settings.json`. Copy of hooks.rs:747-754 (`claude_files`, first entry, as
/// a release build's `claude_settings` at hooks.rs:763 takes it).
fn claude_settings(env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let config = env("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty());
    let home = || env("USERPROFILE").map(|h| PathBuf::from(h).join(".claude"));
    config
        .map(PathBuf::from)
        .or_else(home)
        .map(|d| d.join("settings.json"))
}

/// `knock doctor` against this machine. Exit 0 when every check passes, 1
/// otherwise.
pub fn main() -> ExitCode {
    let paths = Paths::from_env(|k| std::env::var_os(k));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let report = run(&paths, now, TIMEOUT);
    print!("{}", report.render());
    match report.passed() {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    }
}

/// All three checks. `now_ms` is the wall clock for "N minutes ago".
pub fn run(paths: &Paths, now_ms: u64, timeout: Duration) -> Report {
    let spec = ClaudeCode { users: Vec::new() }.hooks();
    let conn = load_server(paths.server.as_deref());
    let reply = match &conn {
        Conn::Found(port, token) => Some(get_health(*port, token, timeout)),
        _ => None,
    };
    Report {
        checks: vec![
            hooks_check(paths.settings.as_deref(), &conn, &spec),
            server_check(&conn, reply.as_ref()),
            last_check(reply.as_ref(), now_ms),
        ],
        info: reply.as_ref().and_then(info),
    }
}

// ---------------------------------------------------------------------------
// server.json

/// What `server.json` gave.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Conn {
    Found(u16, String),
    NoAppData,
    Missing,
    Unreadable,
    Invalid,
}

fn load_server(path: Option<&Path>) -> Conn {
    let Some(path) = path else {
        return Conn::NoAppData;
    };
    match std::fs::read(path) {
        Ok(b) => parse_server(&b).map_or(Conn::Invalid, |(p, t)| Conn::Found(p, t)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Conn::Missing,
        Err(_) => Conn::Unreadable,
    }
}

/// Copy of server.rs:139-151 (`parse_config`): a JSON object with a `port` in
/// 49152-65535 and a `token` of 43 base64url characters; anything else is
/// what the app itself refuses to start with. No BOM allowance, as there.
fn parse_server(bytes: &[u8]) -> Option<(u16, String)> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
    let token = v.get("token")?.as_str()?;
    let base64url = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    (PORTS.contains(&port) && token.len() == 43 && token.bytes().all(base64url))
        .then(|| (port, token.to_owned()))
}

// ---------------------------------------------------------------------------
// Check 1: the Claude Code settings file

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hooks {
    Installed,
    Stale,
    Missing,
}

/// Why the settings file couldn't be checked. Messages are hooks.rs:67-75.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Problem {
    Unreadable,
    NotJson,
    Shape,
}

impl Problem {
    fn message(self) -> &'static str {
        match self {
            Self::Unreadable => {
                "Knock couldn't open your Claude Code settings file, so it didn't change anything. Please try again."
            }
            Self::NotJson => {
                "Your Claude Code settings file isn't valid JSON, so Knock didn't change it. Please fix the file, or ask for help, then try again."
            }
            Self::Shape => {
                "Your Claude Code settings file is laid out in a way Knock doesn't recognise, so Knock didn't change it."
            }
        }
    }
}

fn hooks_check(settings: Option<&Path>, conn: &Conn, spec: &HookSpec) -> Check {
    let fail = |m: &str| check(HOOKS, false, m);
    let (port, token) = match conn {
        Conn::Found(p, t) => (*p, t.as_str()),
        Conn::NoAppData => {
            return fail(
                "Knock couldn't check this, because APPDATA isn't set, so it can't find its connection details.",
            );
        }
        Conn::Missing => {
            return fail(
                "Knock couldn't check this, because it hasn't started on this Windows account yet. Start Knock and run its setup.",
            );
        }
        Conn::Unreadable => {
            return fail(
                "Knock couldn't check this, because it couldn't read its connection details (server.json). Please try again.",
            );
        }
        Conn::Invalid => {
            return fail(
                "Knock couldn't check this, because its connection details (server.json) aren't valid. Open Knock's Settings and choose Repair.",
            );
        }
    };
    let Some(path) = settings else {
        return fail(
            "Knock couldn't tell where your Claude Code settings are, because neither CLAUDE_CONFIG_DIR nor USERPROFILE is set.",
        );
    };
    // The wording of main.rs:362-376 (`run_health_check`), with "from
    // Knock's Settings" since a command line has no setup button.
    match read_settings(path).and_then(|raw| status(raw.as_deref(), spec, port, token)) {
        Ok(Hooks::Installed) => check(
            HOOKS,
            true,
            "Knock is in your Claude Code settings (settings.json).",
        ),
        Ok(Hooks::Stale) => fail(
            "Claude Code still points at Knock's old connection. Run setup again from Knock's Settings.",
        ),
        Ok(Hooks::Missing) => fail(
            "Knock isn't in your Claude Code settings (settings.json), or its entry is out of date. Run setup from Knock's Settings to connect it.",
        ),
        Err(p) => fail(p.message()),
    }
}

/// The file's bytes; `None` if it or its folder doesn't exist. A symbolic
/// link is read at its target; a link to nothing is unreadable, not "no
/// file". Copy of hooks.rs:443-452 (`read`) with the dangling-link rule of
/// hooks.rs:531-539 (`target`).
fn read_settings(path: &Path) -> Result<Option<Vec<u8>>, Problem> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match std::fs::symlink_metadata(path) {
            Ok(_) => Err(Problem::Unreadable),
            Err(_) => Ok(None),
        },
        Err(_) => Err(Problem::Unreadable),
    }
}

/// JSON that keeps every object entry, duplicates included, in file order.
/// Copy of hooks.rs:97-177 (`J`): lookups use the last of duplicate keys,
/// and walking an object visits all of them, as the app's checks do.
#[derive(Clone, Debug, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Num(Number),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn get(&self, key: &str) -> Option<&J> {
        let J::Obj(m) = self else { return None };
        m.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn str(&self) -> Option<&str> {
        if let J::Str(s) = self { Some(s) } else { None }
    }
    fn arr(&self) -> Option<&[J]> {
        if let J::Arr(a) = self { Some(a) } else { None }
    }
    /// As `serde_json::to_value` of the app's `J` (last duplicate wins).
    fn value(&self) -> Value {
        match self {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(*b),
            J::Num(n) => Value::Number(n.clone()),
            J::Str(s) => Value::String(s.clone()),
            J::Arr(a) => Value::Array(a.iter().map(J::value).collect()),
            J::Obj(m) => {
                let mut out = Map::new();
                for (k, v) in m {
                    out.insert(k.clone(), v.value());
                }
                Value::Object(out)
            }
        }
    }
}

impl<'de> Deserialize<'de> for J {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = J;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<J, E> {
                Ok(J::Null)
            }
            fn visit_bool<E>(self, b: bool) -> Result<J, E> {
                Ok(J::Bool(b))
            }
            fn visit_i64<E>(self, n: i64) -> Result<J, E> {
                Ok(J::Num(n.into()))
            }
            fn visit_u64<E>(self, n: u64) -> Result<J, E> {
                Ok(J::Num(n.into()))
            }
            fn visit_f64<E: de::Error>(self, n: f64) -> Result<J, E> {
                Number::from_f64(n)
                    .map(J::Num)
                    .ok_or_else(|| E::custom("not a number"))
            }
            fn visit_str<E>(self, s: &str) -> Result<J, E> {
                Ok(J::Str(s.into()))
            }
            fn visit_string<E>(self, s: String) -> Result<J, E> {
                Ok(J::Str(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element()? {
                    v.push(x);
                }
                Ok(J::Arr(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
                let mut m = Vec::new();
                while let Some(e) = a.next_entry()? {
                    m.push(e);
                }
                Ok(J::Obj(m))
            }
        }
        d.deserialize_any(V)
    }
}

/// The file as JSON; no file is `{}`; a leading UTF-8 BOM is ignored (ADR
/// 0010). Copy of hooks.rs:224-234 (`parse`).
fn parse(raw: Option<&[u8]>) -> Result<J, Problem> {
    let Some(b) = raw else {
        return Ok(J::Obj(Vec::new()));
    };
    let v: J =
        serde_json::from_slice(b.strip_prefix(BOM).unwrap_or(b)).map_err(|_| Problem::NotJson)?;
    match v {
        J::Obj(_) => Ok(v),
        _ => Err(Problem::Shape),
    }
}

/// Whether setup could write Knock's entries into `v`: the `Shape` errors of
/// hooks.rs:240-246 (`strip`) and hooks.rs:279-295 (`install`), which the
/// app's `status` runs first (hooks.rs:727). `hooks`, if present, is an
/// object; each registered event in it, if present, is an array.
fn fits(v: &J, spec: &HookSpec) -> bool {
    match v.get("hooks") {
        None => true,
        Some(hooks @ J::Obj(_)) => (spec.events.iter())
            .all(|(event, _)| hooks.get(event).is_none_or(|g| g.arr().is_some())),
        Some(_) => false,
    }
}

/// The exact §8.2 handler. Copy of hooks.rs:193-203 (`handler`).
fn handler(spec: &HookSpec, port: u16, token: &str) -> Value {
    json!({
        "type": "http",
        "url": format!("{OWNER}{port}{}", spec.path),
        "headers": { TOKEN_HEADER: token },
        "timeout": 1
    })
}

/// §8.2 ownership. Copy of hooks.rs:205-207 (`is_knock`).
fn is_knock(h: &J, spec: &HookSpec) -> bool {
    (h.get("url").and_then(J::str)).is_some_and(|u| u.starts_with(OWNER) && u.contains(spec.path))
}

/// Every handler in every matcher group of every event. Copy of
/// hooks.rs:210-220 (`handlers`).
fn handlers(root: &J) -> impl Iterator<Item = &J> {
    let events: &[(String, J)] = match root.get("hooks") {
        Some(J::Obj(m)) => m,
        _ => &[],
    };
    (events.iter())
        .filter_map(|(_, groups)| groups.arr())
        .flatten()
        .filter_map(|g| g.get("hooks")?.arr())
        .flatten()
}

/// Installed: exactly one Knock handler per event, each the current handler
/// (keys in any order) in a group with the right matcher, and no other Knock
/// entry. Stale: not installed, and some Knock entry differs from the
/// current handler (another port or token). Missing: neither. Copy of
/// hooks.rs:314-327 (`installed`) and hooks.rs:723-736 (`Setup::status`).
fn status(raw: Option<&[u8]>, spec: &HookSpec, port: u16, token: &str) -> Result<Hooks, Problem> {
    let v = parse(raw)?;
    if !fits(&v, spec) {
        return Err(Problem::Shape);
    }
    let want = handler(spec, port, token);
    let ours: Vec<Value> = handlers(&v)
        .filter(|h| is_knock(h, spec))
        .map(J::value)
        .collect();
    let installed = ours.len() == spec.events.len()
        && spec.events.iter().all(|(event, matcher)| {
            let groups = v.get("hooks").and_then(|h| h.get(event)).and_then(J::arr);
            groups.into_iter().flatten().any(|g| {
                g.get("matcher").and_then(J::str) == *matcher
                    && (g.get("hooks").and_then(J::arr))
                        .is_some_and(|hs| hs.iter().any(|h| h.value() == want))
            })
        });
    Ok(if installed {
        Hooks::Installed
    } else if ours.iter().any(|h| *h != want) {
        Hooks::Stale
    } else {
        Hooks::Missing
    })
}

// ---------------------------------------------------------------------------
// Checks 2 and 3: the running app

/// What `GET /v1/health` got.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reply {
    /// `200`, with the body.
    Ok(Vec<u8>),
    /// Nothing answered.
    NoAnswer,
    /// `401`: the token wasn't accepted.
    Unauthorized,
    /// Some other status, or not HTTP.
    Other,
}

/// One `GET /v1/health` to 127.0.0.1:`port` (loopback only) with the token,
/// which never leaves this process except in that header.
fn get_health(port: u16, token: &str, timeout: Duration) -> Reply {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else {
        return Reply::NoAnswer;
    };
    let _ = (
        s.set_read_timeout(Some(timeout)),
        s.set_write_timeout(Some(timeout)),
    );
    let req = format!(
        "GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{TOKEN_HEADER}: {token}\r\nConnection: close\r\n\r\n"
    );
    if s.write_all(req.as_bytes()).is_err() {
        return Reply::NoAnswer;
    }
    let mut raw = Vec::new();
    // A timeout after part of the reply still leaves what arrived.
    let _ = (&mut s).take(REPLY_LIMIT).read_to_end(&mut raw);
    if raw.is_empty() {
        return Reply::NoAnswer;
    }
    // "HTTP/1.1 200 OK"
    let code = (raw.strip_prefix(b"HTTP/1."))
        .and_then(|r| r.get(2..5))
        .and_then(|c| std::str::from_utf8(c).ok());
    match code {
        Some("200") => {
            let body = (raw.windows(4).position(|w| w == b"\r\n\r\n"))
                .map_or(Vec::new(), |i| raw[i + 4..].to_vec());
            Reply::Ok(body)
        }
        Some("401") => Reply::Unauthorized,
        _ => Reply::Other,
    }
}

fn server_check(conn: &Conn, reply: Option<&Reply>) -> Check {
    let fail = |m: &str| check(SERVER, false, m);
    match (conn, reply) {
        // main.rs:377-386, with the Repair step spelled out.
        (_, Some(Reply::Ok(_))) => check(
            SERVER,
            true,
            "Knock is listening for Claude Code, on this computer only.",
        ),
        (_, Some(Reply::NoAnswer)) => fail(
            "Knock isn't listening for Claude Code. If Knock isn't running, start it; if it is, open Knock's Settings and choose Repair.",
        ),
        (_, Some(Reply::Unauthorized)) => fail(
            "Something answered on Knock's connection but didn't accept Knock's token. Restart Knock, then run this again; if that doesn't help, open Knock's Settings and choose Repair.",
        ),
        (_, Some(Reply::Other)) => fail(
            "Something other than Knock answered on Knock's connection. Open Knock's Settings and choose Repair.",
        ),
        (Conn::NoAppData, None) => {
            fail("Knock couldn't find its connection details, because APPDATA isn't set.")
        }
        (Conn::Unreadable, None) => fail(
            "Knock couldn't read its connection details (server.json), so it couldn't check this. Please try again.",
        ),
        (Conn::Invalid, None) => fail(
            "Knock isn't listening for Claude Code, because its connection details (server.json) aren't valid. Open Knock's Settings and choose Repair.",
        ),
        (Conn::Missing | Conn::Found(..), None) => fail(
            "Knock isn't listening for Claude Code: it hasn't started on this Windows account yet. Start Knock, then run this again.",
        ),
    }
}

/// `lastEventAt` from a health body: `Ok(None)` if Knock hasn't heard from
/// Claude Code since it started; `Err` if the body doesn't say.
fn last_event(body: &[u8]) -> Result<Option<u64>, ()> {
    let v: Value = serde_json::from_slice(body).map_err(|_| ())?;
    match v.get("lastEventAt").ok_or(())? {
        Value::Null => Ok(None),
        Value::String(s) => parse_iso(s).map(Some).ok_or(()),
        _ => Err(()),
    }
}

fn last_check(reply: Option<&Reply>, now_ms: u64) -> Check {
    let Some(Reply::Ok(body)) = reply else {
        return check(
            LAST,
            false,
            "Knock couldn't check this, because it isn't listening for Claude Code.",
        );
    };
    // main.rs:387-401.
    match last_event(body) {
        Ok(Some(t)) => check(
            LAST,
            true,
            format!(
                "Knock last heard from Claude Code {}.",
                ago(now_ms.saturating_sub(t))
            ),
        ),
        Ok(None) => check(
            LAST,
            false,
            "Knock hasn't heard from Claude Code since it started. Send a message in a Claude Code window, then check again.",
        ),
        Err(()) => check(
            LAST,
            false,
            "Knock answered, but its reply didn't say when it last heard from Claude Code. Restart Knock, then run this again.",
        ),
    }
}

/// "Knock 0.3.0 is running, 2 sessions." from a health reply. Only a
/// version made of version characters is shown, so nothing else from the
/// reply can reach the output.
fn info(reply: &Reply) -> Option<String> {
    let Reply::Ok(body) = reply else { return None };
    let v: Value = serde_json::from_slice(body).ok()?;
    let version = v.get("version")?.as_str()?;
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+');
    if version.is_empty() || version.len() > 40 || !version.chars().all(plain) {
        return None;
    }
    let n = v.get("sessionCount")?.as_u64()?;
    let s = if n == 1 { "" } else { "s" };
    Some(format!("Knock {version} is running, {n} session{s}."))
}

/// "just now", "5 minutes ago", "1 hour ago", "2 days ago". Copy of
/// main.rs:416-425 (`ago`).
fn ago(ms: u64) -> String {
    let s = ms / 1000;
    let (n, unit) = match s {
        0..60 => return "just now".into(),
        60..3600 => (s / 60, "minute"),
        3600..86_400 => (s / 3600, "hour"),
        _ => (s / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// Ms since the Unix epoch from exactly `knock_core::iso8601`'s format,
/// `2026-10-16T14:02:11.120Z` (civil-to-days from H. Hinnant's public-domain
/// date algorithms).
fn parse_iso(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 24 || s.get(23..) != Some("Z") {
        return None;
    }
    for (i, sep) in [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
    ] {
        if b[i] != sep {
            return None;
        }
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss, ms) = (num(11..13)?, num(14..16)?, num(17..19)?, num(20..23)?);
    if y < 1970 || !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59
    {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let total = ((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms;
    u64::try_from(total).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// Made up, the shape of a real one (43 base64url characters).
    const TOKEN: &str = "doctor-test-token-AAAAAAAAAAAAAAAAAAAAAAAAA";
    const OLD_TOKEN: &str = "doctor-old-token-BBBBBBBBBBBBBBBBBBBBBBBBBB";
    const PORT: u16 = 50123;
    const NOW: u64 = 1_792_166_400_000;
    const SHORT: Duration = Duration::from_millis(500);

    fn spec() -> HookSpec {
        ClaudeCode { users: vec![] }.hooks()
    }

    /// A fresh temp folder; removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let d = std::env::temp_dir().join(format!("knock-doctor-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        /// `<dir>\dot-claude\settings.json`, not created.
        fn settings(&self) -> PathBuf {
            self.0.join("dot-claude").join("settings.json")
        }
        fn write_settings(&self, bytes: &[u8]) {
            std::fs::create_dir_all(self.settings().parent().unwrap()).unwrap();
            std::fs::write(self.settings(), bytes).unwrap();
        }
        /// `<dir>\AppData\Knock\server.json`, not created.
        fn server(&self) -> PathBuf {
            self.0.join("AppData").join("Knock").join("server.json")
        }
        fn write_server(&self, bytes: &[u8]) {
            std::fs::create_dir_all(self.server().parent().unwrap()).unwrap();
            std::fs::write(self.server(), bytes).unwrap();
        }
        fn paths(&self) -> Paths {
            Paths {
                settings: Some(self.settings()),
                server: Some(self.server()),
            }
        }
        /// Every file under the folder with its bytes, for "nothing written".
        fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
            fn walk(p: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
                for e in std::fs::read_dir(p).unwrap() {
                    let p = e.unwrap().path();
                    if p.is_dir() {
                        out.push((p.clone(), Vec::new()));
                        walk(&p, out);
                    } else {
                        out.push((p.clone(), std::fs::read(&p).unwrap()));
                    }
                }
            }
            let mut out = Vec::new();
            walk(&self.0, &mut out);
            out.sort();
            out
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn server_json(port: u16, token: &str) -> String {
        format!("{{\n  \"port\": {port},\n  \"token\": \"{token}\"\n}}")
    }

    fn knock_handler(port: u16, token: &str) -> Value {
        json!({
            "type": "http",
            "url": format!("http://127.0.0.1:{port}/v1/hooks/claude-code"),
            "headers": { "X-Knock-Token": token },
            "timeout": 1
        })
    }

    /// Exactly what setup writes for `port` and `token`, built from the §8.2
    /// list itself (not from the code under test).
    fn installed(port: u16, token: &str) -> Value {
        let events = [
            ("UserPromptSubmit", None),
            ("PreToolUse", Some("*")),
            ("PostToolUse", Some("*")),
            ("PermissionRequest", Some("*")),
            ("Notification", None),
            ("Stop", None),
            ("StopFailure", None),
            ("SubagentStart", None),
            ("SubagentStop", None),
            ("PreCompact", None),
            ("SessionEnd", None),
        ];
        let mut hooks = Map::new();
        for (e, m) in events {
            let mut g = Map::new();
            if let Some(m) = m {
                g.insert("matcher".into(), json!(m));
            }
            g.insert("hooks".into(), json!([knock_handler(port, token)]));
            hooks.insert(e.into(), json!([g]));
        }
        json!({ "hooks": hooks })
    }

    fn bytes(v: &Value) -> Vec<u8> {
        serde_json::to_vec_pretty(v).unwrap()
    }

    /// The hooks check for a settings file holding `v`, with the server at
    /// PORT and TOKEN.
    fn hooks_of(raw: Option<&[u8]>) -> Check {
        let d = Dir::new();
        if let Some(raw) = raw {
            d.write_settings(raw);
        }
        let before = d.snapshot();
        let c = hooks_check(
            Some(&d.settings()),
            &Conn::Found(PORT, TOKEN.into()),
            &spec(),
        );
        assert_eq!(d.snapshot(), before, "doctor wrote something");
        c
    }

    fn status_of(v: &Value) -> Result<Hooks, Problem> {
        status(Some(&bytes(v)), &spec(), PORT, TOKEN)
    }

    const INSTALLED: &str = "Knock is in your Claude Code settings (settings.json).";
    const MISSING: &str = "Knock isn't in your Claude Code settings (settings.json), or its entry is out of date. Run setup from Knock's Settings to connect it.";
    const STALE: &str = "Claude Code still points at Knock's old connection. Run setup again from Knock's Settings.";

    #[test]
    fn the_adapter_lists_the_spec_events() {
        let s = spec();
        assert_eq!(s.path, "/v1/hooks/claude-code");
        assert_eq!(s.events.len(), 11);
        assert!(!s.events.iter().any(|(e, _)| *e == "SessionStart"));
    }

    #[test]
    fn installed_hooks_pass() {
        let c = hooks_of(Some(&bytes(&installed(PORT, TOKEN))));
        assert_eq!(c, check(HOOKS, true, INSTALLED));
        // Key order inside a handler doesn't matter.
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["Stop"][0]["hooks"][0] = json!({
            "timeout": 1,
            "headers": { "X-Knock-Token": TOKEN },
            "url": format!("http://127.0.0.1:{PORT}/v1/hooks/claude-code"),
            "type": "http"
        });
        assert_eq!(status_of(&v), Ok(Hooks::Installed));
    }

    #[test]
    fn a_missing_file_or_folder_is_missing() {
        assert_eq!(hooks_of(None), check(HOOKS, false, MISSING));
        // The folder exists, the file doesn't.
        let d = Dir::new();
        std::fs::create_dir_all(d.settings().parent().unwrap()).unwrap();
        let c = hooks_check(
            Some(&d.settings()),
            &Conn::Found(PORT, TOKEN.into()),
            &spec(),
        );
        assert_eq!(c.detail, MISSING);
        assert!(!d.settings().exists(), "no file created");
    }

    #[test]
    fn a_file_without_knock_entries_is_missing() {
        for raw in [&b"{}"[..], b"{\"model\": \"opus\"}", b"{\"hooks\": {}}"] {
            assert_eq!(hooks_of(Some(raw)), check(HOOKS, false, MISSING), "{raw:?}");
        }
        assert_eq!(status_of(&others()), Ok(Hooks::Missing));
    }

    #[test]
    fn some_events_missing_is_missing() {
        for event in ["Stop", "PreToolUse", "SessionEnd"] {
            let mut v = installed(PORT, TOKEN);
            v["hooks"].as_object_mut().unwrap().remove(event);
            assert_eq!(status_of(&v), Ok(Hooks::Missing), "{event}");
        }
        // A tool event without its "*" matcher.
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["PreToolUse"][0]
            .as_object_mut()
            .unwrap()
            .remove("matcher");
        assert_eq!(status_of(&v), Ok(Hooks::Missing));
        // A non-tool event with a matcher it shouldn't have.
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["Stop"][0]["matcher"] = json!("*");
        assert_eq!(status_of(&v), Ok(Hooks::Missing));
        // A duplicate of the current entry: not exactly one per event.
        let mut v = installed(PORT, TOKEN);
        let stop = v["hooks"]["Stop"].as_array_mut().unwrap();
        stop.push(json!({ "hooks": [knock_handler(PORT, TOKEN)] }));
        assert_eq!(status_of(&v), Ok(Hooks::Missing));
        // A duplicate event key, as the app counts it (both are walked).
        let text = String::from_utf8(bytes(&installed(PORT, TOKEN))).unwrap();
        let stop = format!(
            "\"hooks\": {{\n    \"Stop\": [{{\"hooks\": [{}]}}],",
            knock_handler(PORT, TOKEN)
        );
        let dup = text.replacen("\"hooks\": {", &stop, 1);
        assert_eq!(
            status(Some(dup.as_bytes()), &spec(), PORT, TOKEN),
            Ok(Hooks::Missing)
        );
    }

    #[test]
    fn a_stale_port_is_stale() {
        let c = hooks_of(Some(&bytes(&installed(PORT + 1, TOKEN))));
        assert_eq!(c, check(HOOKS, false, STALE));
        // Only one event left behind on the old port.
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["Stop"][0]["hooks"][0] = knock_handler(49999, TOKEN);
        assert_eq!(status_of(&v), Ok(Hooks::Stale));
    }

    #[test]
    fn a_stale_or_missing_token_is_stale() {
        assert_eq!(status_of(&installed(PORT, OLD_TOKEN)), Ok(Hooks::Stale));
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["Stop"][0]["hooks"][0]
            .as_object_mut()
            .unwrap()
            .remove("headers");
        assert_eq!(status_of(&v), Ok(Hooks::Stale), "no token");
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["Stop"][0]["hooks"][0]["headers"] = json!({});
        assert_eq!(status_of(&v), Ok(Hooks::Stale), "empty headers");
        // Another timeout is not the current handler either.
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["Stop"][0]["hooks"][0]["timeout"] = json!(2);
        assert_eq!(status_of(&v), Ok(Hooks::Stale), "old timeout");
        // An old entry left next to a complete, current set.
        let mut v = installed(PORT, TOKEN);
        let stop = v["hooks"]["Stop"].as_array_mut().unwrap();
        stop.push(json!({ "hooks": [knock_handler(49999, OLD_TOKEN)] }));
        assert_eq!(status_of(&v), Ok(Hooks::Stale));
        // Under an event Knock doesn't register (an old SessionStart entry).
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["SessionStart"] = json!([{ "hooks": [knock_handler(49999, OLD_TOKEN)] }]);
        assert_eq!(status_of(&v), Ok(Hooks::Stale));
    }

    #[test]
    fn invalid_json_fails_with_a_plain_explanation() {
        let not_json = "Your Claude Code settings file isn't valid JSON, so Knock didn't change it. Please fix the file, or ask for help, then try again.";
        let shape = "Your Claude Code settings file is laid out in a way Knock doesn't recognise, so Knock didn't change it.";
        for (bad, want) in [
            (&b"{ \"hooks\": "[..], not_json),
            (b"", not_json),
            (b"{ // comment\n}", not_json),
            (b"\xFF\xFE{}", not_json),
            (b"{\"a\": 1,}", not_json),
            (b"[]", shape),
            (b"\"text\"", shape),
            (b"null", shape),
            (b"{\"hooks\": []}", shape),
            (b"{\"hooks\": {\"Stop\": {}}}", shape),
            (b"{\"hooks\": {\"PreToolUse\": null}}", shape),
        ] {
            assert_eq!(hooks_of(Some(bad)), check(HOOKS, false, want), "{bad:?}");
        }
        // An event Knock doesn't register may have any shape, as in the app.
        let mut v = installed(PORT, TOKEN);
        v["hooks"]["PostToolUseFailure"] = json!({ "odd": true });
        assert_eq!(status_of(&v), Ok(Hooks::Installed));
    }

    #[test]
    fn a_leading_bom_is_ignored() {
        let mut raw = BOM.to_vec();
        raw.extend(bytes(&installed(PORT, TOKEN)));
        assert_eq!(hooks_of(Some(&raw)), check(HOOKS, true, INSTALLED));
        let mut raw = BOM.to_vec();
        raw.extend(b"{}");
        assert_eq!(hooks_of(Some(&raw)).detail, MISSING);
        // Only one, and only at the start.
        let mut raw = BOM.repeat(2);
        raw.extend(b"{}");
        assert!(!hooks_of(Some(&raw)).ok);
    }

    /// Other people's hooks and settings, none of them Knock's.
    fn others() -> Value {
        json!({
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "model": "opus",
            "permissions": { "allow": ["Bash(npm run test:*)"] },
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ],
                "Stop": [
                    { "hooks": [{ "type": "http", "url": "http://localhost:8080/other-tool", "timeout": 5 }] }
                ],
                "PostToolUseFailure": [
                    { "matcher": "Edit", "hooks": [{ "type": "command", "command": "notify.exe", "async": true }] }
                ]
            }
        })
    }

    #[test]
    fn other_peoples_hooks_are_ignored() {
        let mut v = installed(PORT, TOKEN);
        let o = others();
        for (k, x) in o.as_object().unwrap() {
            if k != "hooks" {
                v[k] = x.clone();
            }
        }
        for (event, groups) in o["hooks"].as_object().unwrap() {
            let at = v["hooks"]
                .as_object_mut()
                .unwrap()
                .entry(event.clone())
                .or_insert(json!([]));
            for g in groups.as_array().unwrap() {
                at.as_array_mut().unwrap().insert(0, g.clone());
            }
        }
        // And another tool's handler in the same group as Knock's.
        let shared = v["hooks"]["Notification"][0]["hooks"]
            .as_array_mut()
            .unwrap();
        shared.insert(0, json!({ "type": "command", "command": "beep" }));
        assert_eq!(hooks_of(Some(&bytes(&v))), check(HOOKS, true, INSTALLED));
    }

    #[test]
    fn lookalikes_are_not_knock_entries() {
        let lookalikes = [
            "http://localhost:50123/v1/hooks/claude-code",
            "https://127.0.0.1:50123/v1/hooks/claude-code",
            "http://127.0.0.1:50123/v1/hooks/other",
            "http://127.0.0.2:50123/v1/hooks/claude-code",
            "http://127.0.0.10:50123/v1/hooks/claude-code",
            " http://127.0.0.1:50123/v1/hooks/claude-code",
            "HTTP://127.0.0.1:50123/v1/hooks/claude-code",
            "http://[::1]:50123/v1/hooks/claude-code",
        ];
        for url in lookalikes {
            let h = json!({
                "type": "http",
                "url": url,
                "headers": { "X-Knock-Token": OLD_TOKEN },
                "timeout": 1
            });
            // Next to a complete, current set: still installed, not stale.
            let mut v = installed(PORT, TOKEN);
            v["hooks"]["Stop"][0]["hooks"]
                .as_array_mut()
                .unwrap()
                .push(h.clone());
            assert_eq!(status_of(&v), Ok(Hooks::Installed), "{url}");
            // On their own: missing, not stale.
            let v = json!({ "hooks": { "Stop": [{ "hooks": [h] }] } });
            assert_eq!(status_of(&v), Ok(Hooks::Missing), "{url}");
        }
        // A url that isn't a string isn't Knock's either.
        let v = json!({ "hooks": { "Stop": [{ "hooks": [{ "url": 5 }] }] } });
        assert_eq!(status_of(&v), Ok(Hooks::Missing));
    }

    #[test]
    fn a_link_to_nothing_is_unreadable_not_missing() {
        let d = Dir::new();
        std::fs::create_dir_all(d.settings().parent().unwrap()).unwrap();
        let gone = d.0.join("gone.json");
        if let Err(e) = std::os::windows::fs::symlink_file(&gone, d.settings()) {
            // Needs Developer Mode or admin rights.
            eprintln!("skipped: can't create a symlink here ({:?})", e.kind());
            return;
        }
        let c = hooks_check(
            Some(&d.settings()),
            &Conn::Found(PORT, TOKEN.into()),
            &spec(),
        );
        assert_eq!(c.detail, Problem::Unreadable.message());
        assert!(!gone.exists(), "nothing created at the target");
        // A link to a real file is read at its target.
        std::fs::write(&gone, bytes(&installed(PORT, TOKEN))).unwrap();
        let c = hooks_check(
            Some(&d.settings()),
            &Conn::Found(PORT, TOKEN.into()),
            &spec(),
        );
        assert_eq!(c.detail, INSTALLED);
    }

    #[test]
    fn a_folder_where_the_file_should_be_is_unreadable() {
        let d = Dir::new();
        std::fs::create_dir_all(d.settings()).unwrap();
        let c = hooks_check(
            Some(&d.settings()),
            &Conn::Found(PORT, TOKEN.into()),
            &spec(),
        );
        assert_eq!(c, check(HOOKS, false, Problem::Unreadable.message()));
    }

    #[test]
    fn the_settings_file_follows_claude_config_dir_when_it_is_set() {
        fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
            move |k| {
                (vars.iter())
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        }
        let home = [
            ("USERPROFILE", r"C:\Users\A"),
            ("APPDATA", r"C:\Users\A\AppData\Roaming"),
        ];
        let p = Paths::from_env(env(&home));
        assert_eq!(
            p.settings,
            Some(PathBuf::from(r"C:\Users\A\.claude\settings.json"))
        );
        assert_eq!(
            p.server,
            Some(PathBuf::from(
                r"C:\Users\A\AppData\Roaming\Knock\server.json"
            ))
        );
        let both = [
            ("USERPROFILE", r"C:\Users\A"),
            ("CLAUDE_CONFIG_DIR", r"D:\cfg\claude"),
        ];
        assert_eq!(
            Paths::from_env(env(&both)).settings,
            Some(PathBuf::from(r"D:\cfg\claude\settings.json"))
        );
        let empty = [("USERPROFILE", r"C:\Users\A"), ("CLAUDE_CONFIG_DIR", "")];
        assert_eq!(Paths::from_env(env(&empty)).settings, p.settings);
        let only = [("CLAUDE_CONFIG_DIR", r"D:\cfg")];
        assert_eq!(
            Paths::from_env(env(&only)).settings,
            Some(PathBuf::from(r"D:\cfg\settings.json"))
        );
        let none = Paths::from_env(env(&[("APPDATA", "")]));
        assert_eq!((none.settings, none.server), (None, None));
    }

    #[test]
    fn server_json_is_checked_as_the_app_checks_it() {
        let ok = server_json(PORT, TOKEN);
        assert_eq!(parse_server(ok.as_bytes()), Some((PORT, TOKEN.into())));
        for bad in [
            "{ not json".to_string(),
            "[]".into(),
            server_json(80, TOKEN),
            server_json(49151, TOKEN),
            "{\"port\": 70000, \"token\": \"x\"}".into(),
            server_json(PORT, "short"),
            server_json(PORT, &format!("{}=", &TOKEN[..42])),
            server_json(PORT, &format!("{TOKEN}A")),
            format!("{{\"port\": \"{PORT}\", \"token\": \"{TOKEN}\"}}"),
            format!("{{\"token\": \"{TOKEN}\"}}"),
            format!("\u{feff}{ok}"),
        ] {
            assert_eq!(parse_server(bad.as_bytes()), None, "{bad}");
        }
        let d = Dir::new();
        assert_eq!(load_server(None), Conn::NoAppData);
        assert_eq!(load_server(Some(&d.server())), Conn::Missing);
        d.write_server(b"{}");
        assert_eq!(load_server(Some(&d.server())), Conn::Invalid);
        std::fs::remove_file(d.server()).unwrap();
        std::fs::create_dir(d.server()).unwrap();
        assert_eq!(load_server(Some(&d.server())), Conn::Unreadable);
    }

    /// A stand-in for Knock's server on 127.0.0.1 (std only): answers one
    /// request with `reply` and sends back the request head.
    fn stand_in(reply: String) -> (u16, mpsc::Receiver<String>) {
        let listener = bind_in_range();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut conn = BufReader::new(conn);
            let mut head = String::new();
            while !head.ends_with("\r\n\r\n") {
                if conn.read_line(&mut head).unwrap() == 0 {
                    break;
                }
            }
            let _ = conn.get_mut().write_all(reply.as_bytes());
            let _ = tx.send(head);
        });
        (port, rx)
    }

    /// A loopback listener on a port in the §8.1 range, so a server.json
    /// pointing at it is valid. Each port is tried at most once per test
    /// run, so a test's closed port is never another test's stand-in.
    fn bind_in_range() -> TcpListener {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let seed = std::process::id() as usize * 7919;
        loop {
            let i = NEXT.fetch_add(1, Ordering::Relaxed);
            assert!(i < 16384, "no free port");
            // 104729 is odd, so i -> port is one-to-one over the range.
            let p = 49152 + ((seed + i * 104_729) % 16384) as u16;
            if let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, p)) {
                return l;
            }
        }
    }

    fn http(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn health_body(last: Option<u64>) -> String {
        json!({
            "version": "0.3.0-beta.1",
            "uptimeS": 12,
            "sessionCount": 2,
            "lastEventAt": last.map(knock_core::iso8601),
        })
        .to_string()
    }

    /// A port nothing listens on.
    fn closed_port() -> u16 {
        bind_in_range().local_addr().unwrap().port()
    }

    #[test]
    fn health_is_a_get_with_the_token_to_loopback() {
        let (port, rx) = stand_in(http("200 OK", &health_body(Some(NOW - 5 * 60_000))));
        let r = get_health(port, TOKEN, SHORT);
        let head = rx.recv().unwrap();
        assert!(head.starts_with("GET /v1/health HTTP/1.1\r\n"), "{head}");
        assert!(head.contains(&format!("\r\nX-Knock-Token: {TOKEN}\r\n")));
        assert!(head.contains(&format!("\r\nHost: 127.0.0.1:{port}\r\n")));
        assert!(
            !head.to_ascii_lowercase().contains("content-length"),
            "no body"
        );
        let Reply::Ok(body) = &r else { panic!("{r:?}") };
        assert_eq!(last_event(body), Ok(Some(NOW - 5 * 60_000)));
        assert!(server_check(&Conn::Found(port, TOKEN.into()), Some(&r)).ok);
        assert_eq!(
            last_check(Some(&r), NOW),
            check(
                LAST,
                true,
                "Knock last heard from Claude Code 5 minutes ago."
            )
        );
        assert_eq!(
            info(&r).as_deref(),
            Some("Knock 0.3.0-beta.1 is running, 2 sessions.")
        );
    }

    #[test]
    fn the_server_and_last_event_checks_against_a_stand_in() {
        let conn = |port| Conn::Found(port, TOKEN.to_string());
        let not_listening =
            "Knock couldn't check this, because it isn't listening for Claude Code.";
        // 200, nothing heard yet.
        let (port, _rx) = stand_in(http("200 OK", &health_body(None)));
        let r = get_health(port, TOKEN, SHORT);
        assert_eq!(
            server_check(&conn(port), Some(&r)),
            check(
                SERVER,
                true,
                "Knock is listening for Claude Code, on this computer only."
            )
        );
        assert_eq!(
            last_check(Some(&r), NOW),
            check(
                LAST,
                false,
                "Knock hasn't heard from Claude Code since it started. Send a message in a Claude Code window, then check again."
            )
        );
        // 200, heard just now (a clock a little behind the app's too).
        let (port, _rx) = stand_in(http("200 OK", &health_body(Some(NOW + 2000))));
        let r = get_health(port, TOKEN, SHORT);
        assert_eq!(
            last_check(Some(&r), NOW).detail,
            "Knock last heard from Claude Code just now."
        );
        // 401.
        let (port, _rx) = stand_in(http("401 Unauthorized", ""));
        let r = get_health(port, TOKEN, SHORT);
        assert_eq!(r, Reply::Unauthorized);
        let c = server_check(&conn(port), Some(&r));
        assert!(
            !c.ok && c.detail.contains("didn't accept Knock's token"),
            "{c:?}"
        );
        assert_eq!(last_check(Some(&r), NOW), check(LAST, false, not_listening));
        // Connection refused.
        let port = closed_port();
        let r = get_health(port, TOKEN, SHORT);
        assert_eq!(r, Reply::NoAnswer);
        assert_eq!(
            server_check(&conn(port), Some(&r)),
            check(
                SERVER,
                false,
                "Knock isn't listening for Claude Code. If Knock isn't running, start it; if it is, open Knock's Settings and choose Repair."
            )
        );
        assert_eq!(last_check(Some(&r), NOW), check(LAST, false, not_listening));
        // Something else on the port.
        for reply in [http("404 Not Found", ""), "SSH-2.0-OpenSSH\r\n".into()] {
            let (port, _rx) = stand_in(reply);
            let r = get_health(port, TOKEN, SHORT);
            assert_eq!(r, Reply::Other);
            assert!(!server_check(&conn(port), Some(&r)).ok);
            assert!(!last_check(Some(&r), NOW).ok);
        }
        // Accepts, then closes without a word.
        let (port, _rx) = stand_in(String::new());
        assert_eq!(get_health(port, TOKEN, SHORT), Reply::NoAnswer);
        // 200 with a body that doesn't say: listening, but no last event.
        for body in [
            "",
            "{}",
            "not json",
            r#"{"lastEventAt": 5}"#,
            r#"{"lastEventAt": "yesterday"}"#,
        ] {
            let (port, _rx) = stand_in(http("200 OK", body));
            let r = get_health(port, TOKEN, SHORT);
            assert!(server_check(&conn(port), Some(&r)).ok, "{body}");
            let c = last_check(Some(&r), NOW);
            assert!(
                !c.ok && c.detail.starts_with("Knock answered, but"),
                "{body}"
            );
            assert_eq!(info(&r), None, "{body}");
        }
    }

    #[test]
    fn info_shows_only_a_plain_version() {
        let reply = |v: Value| Reply::Ok(v.to_string().into_bytes());
        let one = reply(json!({ "version": "1.2.3", "sessionCount": 1 }));
        assert_eq!(
            info(&one).as_deref(),
            Some("Knock 1.2.3 is running, 1 session.")
        );
        for bad in ["", "1.0 C:\\Users\\x", "a\nb", &"9".repeat(41), TOKEN] {
            let r = reply(json!({ "version": bad, "sessionCount": 1 }));
            let shown = info(&r);
            assert!(
                shown
                    .as_deref()
                    .is_none_or(|s| !s.contains('\\') && !s.contains('\n')),
                "{bad:?}"
            );
        }
        assert_eq!(info(&Reply::Unauthorized), None);
    }

    /// The whole run on temporary files, against a stand-in.
    fn run_with(d: &Dir, reply: Option<String>) -> Report {
        if let Some(reply) = reply {
            let (port, _rx) = stand_in(reply);
            d.write_server(server_json(port, TOKEN).as_bytes());
            d.write_settings(&bytes(&installed(port, TOKEN)));
        }
        run(&d.paths(), NOW, SHORT)
    }

    #[test]
    fn everything_passes_against_a_healthy_stand_in() {
        let d = Dir::new();
        let report = run_with(
            &d,
            Some(http("200 OK", &health_body(Some(NOW - 3_600_000)))),
        );
        assert!(report.passed(), "{report:?}");
        assert_eq!(
            report.render(),
            "PASS  Connected to Claude Code: Knock is in your Claude Code settings (settings.json).\n\
             PASS  Listening for Claude Code: Knock is listening for Claude Code, on this computer only.\n\
             PASS  Last event from Claude Code: Knock last heard from Claude Code 1 hour ago.\n\
             Knock 0.3.0-beta.1 is running, 2 sessions.\n"
        );
    }

    #[test]
    fn doctor_writes_nothing_anywhere() {
        let d = Dir::new();
        // Each case leaves the folder exactly as it was.
        for step in 0..4 {
            match step {
                1 => d.write_server(server_json(closed_port(), TOKEN).as_bytes()),
                2 => d.write_settings(b"{ broken"),
                3 => d.write_settings(&bytes(&installed(PORT, OLD_TOKEN))),
                _ => {}
            }
            let before = d.snapshot();
            let _ = run(&d.paths(), NOW, SHORT);
            assert_eq!(d.snapshot(), before);
        }
        let report = run_with(&d, Some(http("200 OK", &health_body(None))));
        let before = d.snapshot();
        let _ = run(&d.paths(), NOW, SHORT);
        assert_eq!(d.snapshot(), before);
        assert!(!report.passed());
    }

    /// Every failure, rendered: no token, no folder path, ever (§11.5).
    #[test]
    fn output_never_holds_the_token_or_a_path() {
        let mut outputs = Vec::new();
        let mut add = |r: Report| {
            assert!(!r.passed());
            outputs.push(r.render());
        };
        // No APPDATA, no settings location.
        add(run(&Paths::default(), NOW, SHORT));
        // No server.json.
        let d = Dir::new();
        add(run(&d.paths(), NOW, SHORT));
        // Invalid server.json; unreadable server.json.
        d.write_server(b"{ \"port\": 1 }");
        add(run(&d.paths(), NOW, SHORT));
        std::fs::remove_file(d.server()).unwrap();
        std::fs::create_dir(d.server()).unwrap();
        add(run(&d.paths(), NOW, SHORT));
        std::fs::remove_dir(d.server()).unwrap();
        // Nothing listening, with each kind of settings file.
        d.write_server(server_json(closed_port(), TOKEN).as_bytes());
        for raw in [
            &b"{ \"hooks\": "[..],
            b"[]",
            &bytes(&installed(PORT, OLD_TOKEN)),
            b"{}",
        ] {
            d.write_settings(raw);
            add(run(&d.paths(), NOW, SHORT));
        }
        std::fs::remove_file(d.settings()).unwrap();
        std::fs::create_dir(d.settings()).unwrap();
        add(run(&d.paths(), NOW, SHORT));
        std::fs::remove_dir(d.settings()).unwrap();
        // A stand-in that refuses, one that answers oddly, one that never
        // heard, and one whose reply holds the token and a path.
        for reply in [
            http("401 Unauthorized", TOKEN),
            http("500 Internal Server Error", &format!("{TOKEN} C:\\Users\\x")),
            http("200 OK", &health_body(None)),
            http(
                "200 OK",
                &json!({ "version": TOKEN, "sessionCount": 1, "lastEventAt": d.0.display().to_string() })
                    .to_string(),
            ),
        ] {
            add(run_with(&Dir::new(), Some(reply)));
        }
        let tmp = std::env::temp_dir().display().to_string();
        for out in &outputs {
            let marked = |l: &&str| l.starts_with("PASS  ") || l.starts_with("FAIL  ");
            assert_eq!(out.lines().filter(marked).count(), 3, "{out}");
            assert!(out.lines().any(|l| l.starts_with("FAIL  ")), "{out}");
            for bad in [
                TOKEN,
                OLD_TOKEN,
                "\\",
                "/",
                tmp.as_str(),
                "dot-claude",
                "AppData",
            ] {
                assert!(!out.contains(bad), "{bad:?} in {out}");
            }
        }
    }

    #[test]
    fn iso_times_from_the_app_parse_back() {
        for ms in [
            0,
            1,
            999,
            951_782_400_000,   // 2000-02-29
            1_709_164_800_000, // 2024-02-29
            NOW,
            NOW + 86_399_999,
            4_102_444_800_000,   // 2100-01-01
            253_402_300_799_999, // 9999-12-31T23:59:59.999
        ] {
            assert_eq!(parse_iso(&knock_core::iso8601(ms)), Some(ms), "{ms}");
        }
        for ms in (0..400 * 366).map(|d| d * 86_400_000 + 12_345_678) {
            assert_eq!(parse_iso(&knock_core::iso8601(ms)), Some(ms));
        }
        for bad in [
            "",
            "2026-10-16T14:02:11Z",
            "2026-10-16T14:02:11.120",
            "2026-10-16 14:02:11.120Z",
            "2026-13-16T14:02:11.120Z",
            "2026-10-16T24:02:11.120Z",
            "1969-12-31T23:59:59.999Z",
            "2026-1a-16T14:02:11.120Z",
            "+026-10-16T14:02:11.120Z",
            "2026-10-16T14:02:11.12éZ",
        ] {
            assert_eq!(parse_iso(bad), None, "{bad}");
        }
    }

    #[test]
    fn ago_in_plain_words() {
        let (s, m, h, d) = (1000, 60_000, 3_600_000, 86_400_000);
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59 * s), "just now");
        assert_eq!(ago(m), "1 minute ago");
        assert_eq!(ago(59 * m + 59 * s), "59 minutes ago");
        assert_eq!(ago(h), "1 hour ago");
        assert_eq!(ago(23 * h), "23 hours ago");
        assert_eq!(ago(2 * d), "2 days ago");
    }
}
