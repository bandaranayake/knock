//! `knock` with bad or missing arguments prints usage to stderr and fails.

use std::process::Command;

#[test]
fn bad_or_missing_arguments_print_usage_and_fail() {
    for args in [
        &[][..],
        &["nope"],
        &["fixtures", "sanitize", "x"],
        &["fixtures", "check"],
        &["doctor", "extra"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_knock"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("usage: knock"));
    }
}

#[test]
fn check_of_a_folder_with_no_fixtures_fails() {
    let dir = std::env::temp_dir().join(format!("knock-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_knock"))
        .args(["fixtures", "check"])
        .arg(&dir)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "error: no fixture files found, 0 checked"
    );
}

/// `knock doctor` (FR-12) on temporary files only: `APPDATA`,
/// `CLAUDE_CONFIG_DIR` and `USERPROFILE` all point into a temp folder, so the
/// real settings file and `%APPDATA%\Knock` are never read.
mod doctor {
    use knock_core::{Adapter, ClaudeCode};
    use serde_json::{Map, Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    /// Made up, the shape of a real one (43 base64url characters).
    const TOKEN: &str = "doctor-cli-token-AAAAAAAAAAAAAAAAAAAAAAAAAA";

    struct Home(PathBuf);

    impl Home {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("knock-doctor-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("AppData").join("Knock")).unwrap();
            std::fs::create_dir_all(dir.join("claude")).unwrap();
            Self(dir)
        }
        fn server(&self, port: u16) {
            let cfg = format!(r#"{{"port":{port},"token":"{TOKEN}"}}"#);
            std::fs::write(
                self.0.join("AppData").join("Knock").join("server.json"),
                cfg,
            )
            .unwrap();
        }
        /// Every handler setup writes, for `port`.
        fn installed(&self, port: u16) {
            let spec = ClaudeCode { users: vec![] }.hooks();
            let mut hooks = Map::new();
            for (event, matcher) in spec.events {
                let h = json!({
                    "type": "http",
                    "url": format!("http://127.0.0.1:{port}{}", spec.path),
                    "headers": { "X-Knock-Token": TOKEN },
                    "timeout": 1
                });
                let mut g = Map::new();
                if let Some(m) = matcher {
                    g.insert("matcher".into(), json!(m));
                }
                g.insert("hooks".into(), json!([h]));
                hooks.insert(event.to_string(), json!([g]));
            }
            let v = json!({ "hooks": hooks });
            std::fs::write(self.settings(), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        }
        fn settings(&self) -> PathBuf {
            self.0.join("claude").join("settings.json")
        }
        fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
            fn walk(p: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
                for e in std::fs::read_dir(p).unwrap() {
                    let p = e.unwrap().path();
                    match p.is_dir() {
                        true => walk(&p, out),
                        false => out.push((p.clone(), std::fs::read(&p).unwrap())),
                    }
                }
            }
            let mut out = Vec::new();
            walk(&self.0, &mut out);
            out.sort();
            out
        }
        /// Runs `knock doctor`; checks it wrote nothing and printed no token
        /// or path.
        fn doctor(&self) -> Output {
            let before = self.files();
            let out = Command::new(env!("CARGO_BIN_EXE_knock"))
                .arg("doctor")
                .env("APPDATA", self.0.join("AppData"))
                .env("CLAUDE_CONFIG_DIR", self.0.join("claude"))
                .env("USERPROFILE", self.0.join("profile"))
                .output()
                .unwrap();
            assert_eq!(self.files(), before, "doctor wrote something");
            assert!(!self.0.join("profile").exists());
            let tmp = std::env::temp_dir().display().to_string();
            for s in [&out.stdout, &out.stderr] {
                let s = String::from_utf8_lossy(s);
                for bad in [TOKEN, "\\", tmp.as_str()] {
                    assert!(!s.contains(bad), "{bad:?} in {s}");
                }
            }
            assert!(out.stderr.is_empty());
            out
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A stand-in for Knock on 127.0.0.1, on a port in the §8.1 range, that
    /// answers one request with `status` and `body`.
    fn stand_in(status: &'static str, body: String) -> u16 {
        let seed = std::process::id() as usize * 31;
        let listener = (0..16384)
            .map(|i| 49152 + ((seed + i * 104_729) % 16384) as u16)
            .find_map(|p| TcpListener::bind((Ipv4Addr::LOCALHOST, p)).ok())
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut conn = BufReader::new(conn);
            let mut head = String::new();
            while !head.ends_with("\r\n\r\n") && conn.read_line(&mut head).unwrap() > 0 {}
            let ok = head.starts_with("GET /v1/health HTTP/1.1\r\n")
                && head.contains(&format!("\r\nX-Knock-Token: {TOKEN}\r\n"));
            let (status, body) = if ok {
                (status, body)
            } else {
                ("400 Bad Request", String::new())
            };
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.get_mut().write_all(reply.as_bytes());
        });
        port
    }

    fn stdout_lines(out: &Output) -> Vec<String> {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn all_three_pass_exits_0() {
        let home = Home::new("pass");
        let body: Value = json!({
            "version": "0.3.0",
            "uptimeS": 5,
            "sessionCount": 1,
            "lastEventAt": "2000-01-01T00:00:00.000Z",
        });
        let port = stand_in("200 OK", body.to_string());
        home.server(port);
        home.installed(port);
        let out = home.doctor();
        assert_eq!(out.status.code(), Some(0), "{:?}", stdout_lines(&out));
        let lines = stdout_lines(&out);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].starts_with("PASS  Connected to Claude Code: "));
        assert!(lines[1].starts_with("PASS  Listening for Claude Code: "));
        assert!(lines[2].starts_with("PASS  Last event from Claude Code: Knock last heard"));
        assert!(lines[2].ends_with(" days ago."), "{}", lines[2]);
        assert_eq!(lines[3], "Knock 0.3.0 is running, 1 session.");
    }

    #[test]
    fn any_failure_exits_1() {
        // Knock never started: no server.json, no settings file.
        let out = Home::new("none").doctor();
        assert_eq!(out.status.code(), Some(1));
        let lines = stdout_lines(&out);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines.iter().all(|l| l.starts_with("FAIL  ")), "{lines:?}");
        // Listening, but the hooks are missing and nothing was heard yet.
        let home = Home::new("partial");
        let body =
            json!({ "version": "0.3.0", "uptimeS": 5, "sessionCount": 0, "lastEventAt": null });
        let port = stand_in("200 OK", body.to_string());
        home.server(port);
        std::fs::write(home.settings(), b"{}").unwrap();
        let out = home.doctor();
        assert_eq!(out.status.code(), Some(1));
        let lines = stdout_lines(&out);
        assert!(
            lines[0].starts_with("FAIL  Connected to Claude Code: "),
            "{lines:?}"
        );
        assert!(
            lines[1].starts_with("PASS  Listening for Claude Code: "),
            "{lines:?}"
        );
        assert!(
            lines[2].starts_with("FAIL  Last event from Claude Code: "),
            "{lines:?}"
        );
        // Refused (401).
        let home = Home::new("401");
        let port = stand_in("401 Unauthorized", String::new());
        home.server(port);
        home.installed(port);
        let out = home.doctor();
        assert_eq!(out.status.code(), Some(1));
        assert!(stdout_lines(&out)[1].starts_with("FAIL  Listening for Claude Code: "));
    }
}

mod sim {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::mpsc;

    /// Made up, the shape of a real one (43 base64url characters).
    const TOKEN: &str = "test-token-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    /// A fake `%APPDATA%` whose `Knock\server.json` points at `port`.
    fn appdata(name: &str, port: Option<u16>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("knock-sim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Knock")).unwrap();
        if let Some(port) = port {
            let cfg = format!(r#"{{"port":{port},"token":"{TOKEN}"}}"#);
            std::fs::write(dir.join("Knock").join("server.json"), cfg).unwrap();
        }
        dir
    }

    fn knock(dir: &PathBuf, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_knock"))
            .args(args)
            .env("APPDATA", dir)
            .output()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        for s in [&out.stdout, &out.stderr] {
            assert!(!String::from_utf8_lossy(s).contains(TOKEN), "token printed");
        }
        out
    }

    /// Accepts `n` requests, answers each with `status`, sends back
    /// (head, body) per request.
    fn fake_knock(n: usize, status: &'static str) -> (u16, mpsc::Receiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for conn in listener.incoming().take(n) {
                let mut conn = BufReader::new(conn.unwrap());
                let mut head = String::new();
                while !head.ends_with("\r\n\r\n") {
                    conn.read_line(&mut head).unwrap();
                }
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = vec![0; len];
                conn.read_exact(&mut body).unwrap();
                let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\n\r\n{{}}");
                conn.get_mut().write_all(reply.as_bytes()).unwrap();
                tx.send((head, String::from_utf8(body).unwrap())).unwrap();
            }
        });
        (port, rx)
    }

    #[test]
    fn posts_each_event_with_the_token_and_sim_header_and_skips_controls() {
        let (port, rx) = fake_knock(4, "200 OK");
        let dir = appdata("ok", Some(port));
        let out = knock(&dir, &["sim", "crash", "--speed", "1000"]);
        assert_eq!(out.status.code(), Some(0));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("s41  skipped process_gone"), "{stdout}");
        let scenario = include_str!("../scenarios/crash.jsonl");
        let payloads: Vec<String> = scenario
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v.get("control").is_none())
            .map(|v| v["payload"].to_string())
            .collect();
        assert_eq!(payloads.len(), 4);
        for want in payloads {
            let (head, body) = rx.recv().unwrap();
            assert!(head.starts_with("POST /v1/hooks/claude-code HTTP/1.1\r\n"));
            assert!(head.contains(&format!("\r\nX-Knock-Token: {TOKEN}\r\n")));
            assert!(head.contains("\r\nX-Knock-Sim: 1\r\n"));
            assert_eq!(body, want);
        }
    }

    #[test]
    fn says_plainly_when_knock_is_not_running_or_refuses() {
        let no_file = knock(&appdata("none", None), &["sim", "demo"]);
        let closed = {
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            knock(&appdata("closed", Some(port)), &["sim", "demo"])
        };
        let (port, _rx) = fake_knock(1, "401 Unauthorized");
        let refused = knock(&appdata("401", Some(port)), &["sim", "demo"]);
        for (out, want) in [
            (no_file, "error: Knock doesn't seem to be running"),
            (
                closed,
                "error: Knock isn't running: nothing answered on 127.0.0.1:",
            ),
            (refused, "error: Knock refused the request"),
        ] {
            assert_eq!(out.status.code(), Some(1));
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.starts_with(want), "{err}");
        }
        let bad = knock(&appdata("speed", None), &["sim", "demo", "--speed", "0"]);
        assert!(String::from_utf8_lossy(&bad.stderr).contains("--speed must be a number above 0"));
        let unknown = knock(&appdata("unknown", None), &["sim", "nope", "--local"]);
        assert!(String::from_utf8_lossy(&unknown.stderr).starts_with("error: no scenario `nope`"));
    }
}
