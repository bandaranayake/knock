//! Copy diagnostics (SPEC.md §8.5 `get_diagnostics()`, §11 rule 8, ADR
//! 0038): the plain text the user pastes into a bug report. The UI copies it
//! unchanged and nothing is ever sent anywhere.
//!
//! The text holds only what ADR 0038 lists: Knock's version, the Windows
//! version and build, the health check results, how many sessions are in each
//! tier and at each jump precision, nine settings, and each display's
//! scaling. It never holds agent content, the token, the port, a session id,
//! a project name, a cwd hash, a user name, or any path beyond a file name,
//! because testers paste it into public GitHub issues.
//!
//! Everything here is pure: `get_diagnostics()` gathers the facts and this
//! module turns them into words, so the words are tested without a window.

use crate::Check;
use crate::jump::Precision;
use crate::pipeline::SessionView;
use crate::platform::{Display, OsVersion};
use crate::settings::Settings;

/// What the text is made of, gathered by `get_diagnostics()`.
pub struct Facts<'a> {
    /// Knock's own version, like "0.3.0-beta.1".
    pub version: &'a str,
    /// `None` when Windows doesn't say which version it is.
    pub os: Option<OsVersion>,
    /// `run_health_check()`'s own results, not a second copy of the checks.
    pub checks: &'a [Check],
    pub sessions: &'a [SessionView],
    pub settings: &'a Settings,
    pub displays: &'a [Display],
}

/// "on" or "off", for a setting that is one or the other.
fn on_off(b: bool) -> &'static str {
    match b {
        true => "on",
        false => "off",
    }
}

/// The Windows line, or a plain line saying Windows didn't say.
fn os_line(os: Option<&OsVersion>) -> String {
    let Some(os) = os else {
        return "Windows version: Windows didn't say".into();
    };
    let version = match os.version.is_empty() {
        true => String::new(),
        false => format!(", version {}", os.version),
    };
    format!("{}{version} (build {})", os.name, os.build)
}

/// How many sessions are in each tier (§8.4), in plain words. Every tier is
/// named even at 0, so a reader can see the shape at a glance.
fn tiers(sessions: &[SessionView]) -> String {
    let count =
        |tier: knock_core::Tier| sessions.iter().filter(|s| s.tier == tier.as_str()).count();
    use knock_core::Tier::*;
    format!(
        "needs you {}, working {}, done {}, resting {}",
        count(NeedsYou),
        count(Working),
        count(Ready),
        count(Resting)
    )
}

/// How many sessions Knock could find a window for (§9.5), in plain words.
fn precisions(sessions: &[SessionView]) -> String {
    let count = |p: Precision| sessions.iter().filter(|s| s.precision == p).count();
    format!(
        "exact window {}, app window {}, none {}",
        count(Precision::ExactWindow),
        count(Precision::AppWindow),
        count(Precision::None)
    )
}

/// A display's scaling as Windows shows it: 1.5 is "150%".
fn scaling(scale: f64) -> String {
    format!("{}%", (scale * 100.0).round())
}

/// The §8.5 reply: `{ version, text }`. The UI drops the lot unless both
/// are non-empty strings (`diagnostics.ts`), which they always are.
pub fn reply(f: &Facts) -> serde_json::Value {
    serde_json::json!({ "version": f.version, "text": text(f) })
}

/// The whole text, ready to paste. Lines are joined with "\n" and the text
/// ends with one, so pasting it into an issue keeps its shape.
pub fn text(f: &Facts) -> String {
    let s = f.settings;
    let mut out = vec![
        format!("Knock {}", f.version),
        os_line(f.os.as_ref()),
        String::new(),
        "Checks".into(),
    ];
    for c in f.checks {
        let result = match c.ok {
            true => "pass",
            false => "fail",
        };
        out.push(format!("  {}: {result} - {}", c.label, c.detail));
    }
    out.push(String::new());
    out.push(format!("Sessions {}", f.sessions.len()));
    out.push(format!("  {}", tiers(f.sessions)));
    out.push(format!("  jump target: {}", precisions(f.sessions)));
    out.push(String::new());
    out.push("Settings".into());
    let taskbar = match s.monitor == "primary" {
        true => "main display",
        false => "a chosen display",
    };
    out.push(format!(
        "  show as {}, size {}, taskbar {taskbar}",
        label(s.mode),
        label(s.size)
    ));
    out.push(format!(
        "  stuck after {} min, remind after {} min, notify when stuck {}",
        s.stall_minutes,
        s.escalation_minutes,
        on_off(s.notify_on_stall)
    ));
    out.push(format!(
        "  notifications paused {}, quiet hours {}, start at sign-in {}",
        on_off(s.pause_notifications),
        on_off(s.quiet_hours.enabled),
        sign_in(s.launch_at_sign_in)
    ));
    out.push(String::new());
    out.push(format!("Displays {}", f.displays.len()));
    for (i, d) in f.displays.iter().enumerate() {
        let main = match d.primary {
            true => " (main)",
            false => "",
        };
        out.push(format!("  display {}{main}: {}", i + 1, scaling(d.scale)));
    }
    out.push(String::new());
    out.join("\n")
}

/// `launchAtSignIn` is three-valued: `null` until setup asks (§8.6), and
/// while it is `null` setup still opens by itself (ADR 0039), so the text
/// says which of the three it is.
fn sign_in(setting: Option<bool>) -> &'static str {
    match setting {
        Some(true) => "on",
        Some(false) => "off",
        None => "not chosen yet",
    }
}

/// A §8.6 enum as the word Settings shows the user.
fn label<T: serde::Serialize>(v: T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{NeedsView, SessionView};
    use crate::settings::{Mode, QuietHours, Settings, Size};
    use crate::strip::Rect;

    fn check(label: &'static str, ok: bool, detail: &str) -> Check {
        Check {
            id: "x",
            label,
            ok,
            detail: detail.into(),
        }
    }

    /// A session as the pipeline publishes it, with every field that could
    /// carry something private set to a distinctive string.
    fn session(id: &str, tier: knock_core::Tier, precision: Precision) -> SessionView {
        SessionView {
            id: id.into(),
            project: "SECRETPROJECT".into(),
            tier: tier.as_str(),
            state: "working",
            since: "2026-11-18T09:00:00.000Z".into(),
            activity: None,
            needs: Some(NeedsView {
                kind: "permission",
                label: Some("SECRETTOOL".into()),
                seen: false,
            }),
            precision,
            animal: "fox".into(),
            simulated: false,
        }
    }

    fn display(primary: bool, scale: f64) -> Display {
        Display {
            id: "0123456789abcdef".into(),
            primary,
            monitor: Rect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            },
            work_area: Rect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1392,
            },
            scale,
        }
    }

    fn settings() -> Settings {
        Settings {
            monitor: "ce99b1e1b13dd0b5".into(),
            mode: Mode::Plain,
            size: Size::Large,
            stall_minutes: 12,
            escalation_minutes: 7,
            notify_on_stall: true,
            pause_notifications: true,
            quiet_hours: QuietHours {
                enabled: true,
                start: "22:00".into(),
                end: "08:00".into(),
            },
            launch_at_sign_in: Some(false),
            ..Settings::default()
        }
    }

    /// Every item ADR 0038 lists, on a machine with something of everything.
    #[test]
    fn the_text_holds_every_item_adr_0038_lists() {
        let checks = [
            check(
                "Connected to Claude Code",
                true,
                "Knock is in settings.json.",
            ),
            check(
                "Listening for Claude Code",
                false,
                "Choose Repair to fix it.",
            ),
        ];
        let sessions = [
            session("a", knock_core::Tier::NeedsYou, Precision::ExactWindow),
            session("b", knock_core::Tier::Working, Precision::AppWindow),
            session("c", knock_core::Tier::Working, Precision::None),
        ];
        let settings = settings();
        let displays = [display(true, 1.5), display(false, 1.0)];
        let got = text(&Facts {
            version: "0.3.0-beta.1",
            os: Some(OsVersion {
                name: "Windows 11",
                version: "25H2".into(),
                build: "26200.1742".into(),
            }),
            checks: &checks,
            sessions: &sessions,
            settings: &settings,
            displays: &displays,
        });
        assert_eq!(
            got,
            "\
Knock 0.3.0-beta.1
Windows 11, version 25H2 (build 26200.1742)

Checks
  Connected to Claude Code: pass - Knock is in settings.json.
  Listening for Claude Code: fail - Choose Repair to fix it.

Sessions 3
  needs you 1, working 2, done 0, resting 0
  jump target: exact window 1, app window 1, none 1

Settings
  show as plain, size large, taskbar a chosen display
  stuck after 12 min, remind after 7 min, notify when stuck on
  notifications paused on, quiet hours on, start at sign-in off

Displays 2
  display 1 (main): 150%
  display 2: 100%
"
        );
    }

    /// The defaults, nothing running, and a Windows that doesn't say which
    /// version it is: still every label, and still readable.
    #[test]
    fn the_text_on_a_quiet_machine_names_every_item_anyway() {
        let got = text(&Facts {
            version: "0.3.0-beta.1",
            os: None,
            checks: &[],
            sessions: &[],
            settings: &Settings::default(),
            displays: &[display(true, 1.0)],
        });
        assert_eq!(
            got,
            "\
Knock 0.3.0-beta.1
Windows version: Windows didn't say

Checks

Sessions 0
  needs you 0, working 0, done 0, resting 0
  jump target: exact window 0, app window 0, none 0

Settings
  show as animals, size medium, taskbar main display
  stuck after 10 min, remind after 5 min, notify when stuck off
  notifications paused off, quiet hours off, start at sign-in not chosen yet

Displays 1
  display 1 (main): 100%
"
        );
        // A Windows that gives a build but no display version.
        let os = Some(OsVersion {
            name: "Windows 10",
            version: String::new(),
            build: "19045".into(),
        });
        assert_eq!(os_line(os.as_ref()), "Windows 10 (build 19045)");
    }

    /// §11 rules 3 and 4, and ADR 0038: none of the things the text must
    /// never hold. The facts here carry every one of them that [`Facts`] can
    /// carry: the project name, a tool label (agent content), two session
    /// ids, the monitor id and an `animalOverrides` key (both cwd-style
    /// hashes) and a display id. The token, the port and the user name have
    /// no field at all, so they can only ever arrive inside a quoted
    /// message; they are checked for too, as a guard for later changes.
    #[test]
    fn the_text_holds_no_token_id_project_hash_name_or_path() {
        let token = "TOKENTOKENTOKENTOKENTOKENTOKENTOKENTOKENTOK";
        let settings = Settings {
            // A real monitor id: a hash, so it must not be in the text.
            monitor: "ce99b1e1b13dd0b5".into(),
            animal_overrides: [("0f1e2d3c4b5a6978".to_string(), "fox".to_string())]
                .into_iter()
                .collect(),
            launch_at_sign_in: Some(true),
            ..Settings::default()
        };
        let checks = [
            // The health check's own worst case: a problem message.
            check(
                "Connected to Claude Code",
                false,
                crate::hooks::Problem::NotJson.message(),
            ),
            check("Last event from Claude Code", true, "just now"),
        ];
        let sessions = [
            session(
                "SESSION-1234-ABCD",
                knock_core::Tier::NeedsYou,
                Precision::None,
            ),
            session(
                "SESSION-5678-EFGH",
                knock_core::Tier::Ready,
                Precision::AppWindow,
            ),
        ];
        let got = text(&Facts {
            version: "0.3.0-beta.1",
            os: Some(OsVersion {
                name: "Windows 11",
                version: "25H2".into(),
                build: "26200.1742".into(),
            }),
            checks: &checks,
            sessions: &sessions,
            settings: &settings,
            displays: &[display(true, 2.0)],
        });
        for secret in [
            token,
            "SECRETPROJECT",
            "SECRETTOOL",
            "SESSION-1234-ABCD",
            "SESSION-5678-EFGH",
            "ce99b1e1b13dd0b5",
            "0f1e2d3c4b5a6978",
            "0123456789abcdef",
            // A user name and a port, in the shapes they would appear in.
            "A B",
            "50123",
            "49152",
        ] {
            assert!(
                !got.contains(secret),
                "{secret} is in the diagnostics:\n{got}"
            );
        }
        // No path of any kind: a file name has no separator in it.
        assert!(!got.contains('\\'), "a Windows path:\n{got}");
        assert!(!got.contains('/'), "a path:\n{got}");
        // Nor the window geometry, which would say which displays are where.
        assert!(!got.contains("2560"), "{got}");
        assert!(!got.contains("1392"), "{got}");
    }

    /// Every health-check detail and every setup problem can reach the text,
    /// so none of them may hold a path either (§11 rule 5).
    #[test]
    fn no_message_the_text_can_quote_holds_a_path() {
        use crate::hooks::Problem::*;
        let problems = [
            NoPath,
            NoServer,
            Unreadable,
            NotJson,
            Shape,
            NoPreview,
            Stale,
            Backup,
            Write,
            NothingToRemove,
            Linked,
            RealFile,
        ];
        for p in problems {
            let m = p.message();
            assert!(!m.contains('\\') && !m.contains('/'), "{p:?}: {m}");
        }
        // Both halves of the no-path message (a development build, and a
        // release build that can't find the user folder).
        for debug in [true, false] {
            let m = crate::hooks::no_path_message(debug);
            assert!(!m.contains('\\') && !m.contains('/'), "{m}");
        }
    }

    /// §8.5: `{ version, text }`, both non-empty strings, and the version is
    /// the app's one version (the same as `GET /v1/health` reports).
    #[test]
    fn the_reply_is_a_version_and_a_text_the_ui_accepts() {
        let settings = Settings::default();
        let got = reply(&Facts {
            version: env!("CARGO_PKG_VERSION"),
            os: None,
            checks: &[],
            sessions: &[],
            settings: &settings,
            displays: &[],
        });
        let field = |k: &str| got[k].as_str().expect("a string").to_owned();
        assert_eq!(field("version"), env!("CARGO_PKG_VERSION"));
        assert!(!field("version").trim().is_empty());
        assert!(field("text").starts_with(&format!("Knock {}", field("version"))));
        assert!(field("text").lines().count() > 10, "{}", field("text"));
        assert_eq!(got.as_object().map(|o| o.len()), Some(2), "no other field");
    }

    #[test]
    fn scaling_reads_as_windows_shows_it() {
        assert_eq!(scaling(1.0), "100%");
        assert_eq!(scaling(1.25), "125%");
        assert_eq!(scaling(1.5), "150%");
        assert_eq!(scaling(1.75), "175%");
        assert_eq!(scaling(2.0), "200%");
        // An odd DPI rounds to a whole percent.
        assert_eq!(scaling(1.3333), "133%");
    }
}
