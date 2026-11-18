//! Reading `%APPDATA%\Knock\sessions.json` back at start (FR-13, §8.4
//! "Restore", ADR 0037). `Core::persist` writes it; this module is the only
//! reader, so the two must stay in step.
//!
//! The file holds §11.4 data only: no agent ids (ADR 0019), no cwd (only its
//! hash), no window titles or handles, no free text. Nothing here reads a
//! clock or the OS: the caller passes the restore time and the re-checked
//! liveness (§7.3).
//!
//! ADR 0037 item 7: a missing, unreadable or invalid file, an unknown
//! `version`, or one entry that can't be understood means no sessions at all.
//! The whole file's content is dropped rather than guessed at, only the kind
//! of problem is logged (§11.5), and the next save replaces the file.

use crate::jump::{Precision, ProcessId};
use knock_core::{Category, Liveness, NeedsKind, Saved, SavedNeeds, State};
use serde::Deserialize;
use std::path::Path;

/// The file name, under `%APPDATA%\Knock`. Logged as-is: it is not a path.
pub const FILE: &str = "sessions.json";

/// The only `version` this build understands. A later version that changes
/// what a field means bumps it; added fields do not.
const VERSION: u64 = 1;

/// Why no sessions were restored. Kinds only: never content, never a path
/// beyond the file name (§11.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// No file yet: the normal first start.
    Missing,
    Unreadable,
    /// Not JSON, not the expected shape, or a field of the wrong type.
    Invalid,
    /// `version` is missing or not [`VERSION`].
    Version,
    /// An entry Knock can't understand: an unknown state, activity, needs
    /// kind or precision.
    Entry,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    sessions: Vec<Entry>,
}

/// One saved session. Unknown fields are ignored, so a file a later version
/// wrote still restores; a field this version added is optional, so a file
/// an earlier version wrote restores with safe defaults.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    project: String,
    project_key: String,
    /// [`State::as_str`].
    state: String,
    since_ms: u64,
    last_event_ms: u64,
    activity: Option<Category>,
    needs: Option<Needs>,
    animal: String,
    /// ADR 0021: the main agent's turn was done, so a restored episode
    /// closes to `ready`. Added for the restore; a file written without it
    /// means "not done", which only ever closes an episode to `working`.
    #[serde(default)]
    turn_done: bool,
    /// Never written (a simulated session is left out of the file), but
    /// refused here too: none is ever restored (§8.1, ADR 0037 item 1).
    #[serde(default)]
    simulated: bool,
    process: Option<Process>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Needs {
    /// "permission" or "answer".
    kind: String,
    label: Option<String>,
    seen: bool,
    opened_ms: u64,
    escalated: bool,
}

/// The session's process and window as §9.4 found them.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Process {
    pub pid: u32,
    /// Creation time, so a reused PID is not mistaken for it (§9.7).
    pub start: u64,
    pub owner: Option<String>,
    pub precision: Precision,
}

impl Process {
    pub fn id(&self) -> ProcessId {
        ProcessId {
            pid: self.pid,
            start: self.start,
        }
    }
}

/// One session read back: what the engine restores, plus the process the
/// caller re-checked.
#[derive(Debug, PartialEq, Eq)]
pub struct Restorable {
    pub saved: Saved,
    /// `None` when none was recorded ([`Liveness::Unknown`]).
    pub process: Option<Process>,
}

/// Reads and checks `dir/sessions.json`. `alive(p)`: whether `p` is still
/// running, asked once per recorded process (§9.7, the caller's platform
/// call). Every entry is understood or none is returned.
pub fn read(dir: &Path, alive: impl Fn(&Process) -> bool) -> Result<Vec<Restorable>, Problem> {
    let bytes = match std::fs::read(dir.join(FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Problem::Missing),
        Err(_) => return Err(Problem::Unreadable),
    };
    let file: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Problem::Invalid)?;
    if !file.is_object() {
        return Err(Problem::Invalid);
    }
    // The version is checked before any other field is looked at, so a file
    // a later version wrote is never reported as invalid.
    if file.get("version").and_then(serde_json::Value::as_u64) != Some(VERSION) {
        return Err(Problem::Version);
    }
    let file: File = serde_json::from_value(file).map_err(|_| Problem::Invalid)?;
    file.sessions
        .into_iter()
        .map(|e| e.restorable(&alive))
        .collect()
}

impl Entry {
    fn restorable(self, alive: &impl Fn(&Process) -> bool) -> Result<Restorable, Problem> {
        let state = State::parse(&self.state).ok_or(Problem::Entry)?;
        let needs = match self.needs {
            Some(n) => Some(SavedNeeds {
                kind: needs_kind(&n.kind).ok_or(Problem::Entry)?,
                label: n.label,
                seen: n.seen,
                opened: n.opened_ms,
                escalated: n.escalated,
            }),
            None => None,
        };
        // §9.7: the same PID and the same start time, or it is another process.
        let process = self.process;
        let liveness = match &process {
            None => Liveness::Unknown,
            Some(p) if alive(p) => Liveness::Alive,
            Some(_) => Liveness::Gone,
        };
        Ok(Restorable {
            saved: Saved {
                id: self.id,
                project: self.project,
                project_key: self.project_key,
                state,
                since: self.since_ms,
                last_event: self.last_event_ms,
                activity: self.activity,
                needs,
                animal: self.animal,
                turn_done: self.turn_done,
                simulated: self.simulated,
                process: liveness,
            },
            // A process that is gone is not kept: there is nothing to jump
            // to, and the session is already `ended_unexpectedly`.
            process: process.filter(|_| liveness == Liveness::Alive),
        })
    }
}

/// The reverse of `pipeline::needs_kind`.
fn needs_kind(s: &str) -> Option<NeedsKind> {
    match s {
        "permission" => Some(NeedsKind::Permission),
        "answer" => Some(NeedsKind::Answer),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const T0: u64 = 1_792_166_400_000;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("knock-snap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(d: &Path, v: &Value) {
        std::fs::write(d.join(FILE), serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    /// A full v1 entry, as `Core::persist` writes it.
    fn entry() -> Value {
        json!({
            "id": "s1",
            "project": "demo",
            "projectKey": "0123456789abcdef",
            "state": "needs_input",
            "sinceMs": T0,
            "lastEventMs": T0 + 1000,
            "activity": "run",
            "needs": {
                "kind": "permission",
                "label": "PowerShell",
                "seen": true,
                "openedMs": T0,
                "escalated": false,
            },
            "animal": "owl",
            "turnDone": true,
            "process": { "pid": 7596, "start": 185, "owner": "WindowsTerminal.exe", "precision": "app_window" },
        })
    }

    fn file(sessions: Value) -> Value {
        json!({ "version": 1, "savedMs": T0 + 2000, "sessions": sessions })
    }

    fn all(d: &Path) -> Result<Vec<Restorable>, Problem> {
        read(d, |_| true)
    }

    #[test]
    fn a_full_v1_entry_reads_back_completely() {
        let d = dir("full");
        write(&d, &file(json!([entry()])));
        let got = all(&d).unwrap();
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!(r.saved.id, "s1");
        assert_eq!(r.saved.project, "demo");
        assert_eq!(r.saved.project_key, "0123456789abcdef");
        assert_eq!(r.saved.state, State::NeedsInput);
        assert_eq!((r.saved.since, r.saved.last_event), (T0, T0 + 1000));
        assert_eq!(r.saved.activity, Some(Category::Run));
        assert_eq!(r.saved.animal, "owl");
        assert!(r.saved.turn_done && !r.saved.simulated);
        assert_eq!(r.saved.process, Liveness::Alive);
        let n = r.saved.needs.clone().unwrap();
        assert_eq!(
            (n.kind, n.label.as_deref(), n.seen, n.opened, n.escalated),
            (NeedsKind::Permission, Some("PowerShell"), true, T0, false)
        );
        let p = r.process.clone().unwrap();
        assert_eq!(
            p.id(),
            ProcessId {
                pid: 7596,
                start: 185
            }
        );
        assert_eq!(
            (p.owner.as_deref(), p.precision),
            (Some("WindowsTerminal.exe"), Precision::AppWindow)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The fields the restore added are optional: a file the released
    /// version wrote still restores, with safe defaults.
    #[test]
    fn an_old_format_file_without_the_new_fields_still_restores() {
        let d = dir("old");
        let mut e = entry();
        let o = e.as_object_mut().unwrap();
        o.remove("turnDone");
        o.remove("simulated");
        write(&d, &file(json!([e])));
        let got = all(&d).unwrap();
        assert!(!got[0].saved.turn_done, "closes to working, never to ready");
        assert!(!got[0].saved.simulated);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn liveness_is_the_pid_and_the_start_time() {
        let d = dir("live");
        write(&d, &file(json!([entry()])));
        // Same PID, same start time.
        let got = read(&d, |p| {
            p.id()
                == ProcessId {
                    pid: 7596,
                    start: 185,
                }
        })
        .unwrap();
        assert_eq!(got[0].saved.process, Liveness::Alive);
        assert!(got[0].process.is_some());
        // Gone, or the PID reused by a process with another start time.
        let got = read(&d, |_| false).unwrap();
        assert_eq!(got[0].saved.process, Liveness::Gone);
        assert!(got[0].process.is_none(), "nothing to jump to");
        // No process recorded: never checked.
        let mut e = entry();
        e.as_object_mut().unwrap().remove("process");
        write(&d, &file(json!([e])));
        let got = read(&d, |_| panic!("nothing to check")).unwrap();
        assert_eq!(got[0].saved.process, Liveness::Unknown);
        assert!(got[0].process.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_unreadable_invalid_or_wrong_version_file_restores_nothing() {
        let d = dir("bad");
        assert_eq!(all(&d), Err(Problem::Missing));
        for bad in [
            "",
            "   ",
            "not json",
            "[]",
            "null",
            "{\"version\":1,",
            "\u{feff}{}",
        ] {
            std::fs::write(d.join(FILE), bad).unwrap();
            assert_eq!(all(&d).err(), Some(Problem::Invalid), "{bad:?}");
        }
        for v in [json!(null), json!(0), json!(2), json!("1"), json!(1.5)] {
            write(&d, &json!({ "version": v, "sessions": [entry()] }));
            assert_eq!(all(&d).err(), Some(Problem::Version), "{v}");
        }
        write(&d, &json!({ "sessions": [entry()] }));
        assert_eq!(all(&d).err(), Some(Problem::Version), "no version");
        // A version 1 file with no sessions key, or an empty list.
        write(&d, &json!({ "version": 1 }));
        assert_eq!(all(&d).unwrap().len(), 0);
        write(&d, &file(json!([])));
        assert_eq!(all(&d).unwrap().len(), 0);
        // A directory in the file's place: unreadable, not missing.
        std::fs::remove_file(d.join(FILE)).unwrap();
        std::fs::create_dir(d.join(FILE)).unwrap();
        assert_eq!(all(&d).err(), Some(Problem::Unreadable));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Item 7: one entry that can't be understood drops the whole file.
    #[test]
    fn one_entry_that_cant_be_understood_drops_them_all() {
        let d = dir("entry");
        let drop = |d: &Path, f: &dyn Fn(&mut serde_json::Map<String, Value>)| {
            let mut bad = entry();
            f(bad.as_object_mut().unwrap());
            write(d, &file(json!([entry(), bad])));
            all(d).err()
        };
        // An unknown or renamed state.
        for state in [json!("knocking"), json!(""), json!("NeedsInput"), json!(3)] {
            let got = drop(&d, &|o| {
                o.insert("state".into(), state.clone());
            });
            assert!(got.is_some(), "state {state}");
        }
        assert_eq!(
            drop(&d, &|o| {
                o.insert("state".into(), json!("ready"));
                o.remove("needs");
                o.insert("needs".into(), Value::Null);
            }),
            None,
            "ready without needs is fine"
        );
        // An unknown needs kind, or needs_input without its episode.
        let e = Some(Problem::Entry);
        assert_eq!(
            drop(&d, &|o| {
                o["needs"]["kind"] = json!("question");
            }),
            e
        );
        // Missing or wrong-typed required fields.
        for key in [
            "id",
            "project",
            "projectKey",
            "state",
            "sinceMs",
            "lastEventMs",
            "animal",
        ] {
            assert_eq!(
                drop(&d, &|o| {
                    o.remove(key);
                }),
                Some(Problem::Invalid),
                "missing {key}"
            );
            assert_eq!(
                drop(&d, &|o| {
                    o.insert(key.into(), json!(["x"]));
                }),
                Some(Problem::Invalid),
                "wrong type {key}"
            );
        }
        for key in ["kind", "seen", "openedMs", "escalated"] {
            assert_eq!(
                drop(&d, &|o| {
                    o["needs"].as_object_mut().unwrap().remove(key);
                }),
                Some(Problem::Invalid),
                "missing needs.{key}"
            );
        }
        // An unknown activity or precision.
        assert_eq!(
            drop(&d, &|o| {
                o.insert("activity".into(), json!("sleep"));
            }),
            Some(Problem::Invalid)
        );
        assert_eq!(
            drop(&d, &|o| {
                o["process"]["precision"] = json!("tab");
            }),
            Some(Problem::Invalid)
        );
        // A process without its start time can't be re-checked.
        assert_eq!(
            drop(&d, &|o| {
                o["process"].as_object_mut().unwrap().remove("start");
            }),
            Some(Problem::Invalid)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A newer version's extra fields are ignored, not refused, as long as
    /// `version` still says 1.
    #[test]
    fn unknown_fields_are_ignored() {
        let d = dir("extra");
        let mut e = entry();
        let o = e.as_object_mut().unwrap();
        o.insert("somethingNew".into(), json!({ "a": [1, 2] }));
        o["needs"]["alsoNew"] = json!(true);
        o["process"]["window"] = json!(12345);
        let mut f = file(json!([e]));
        f.as_object_mut()
            .unwrap()
            .insert("extra".into(), json!("x"));
        write(&d, &f);
        assert_eq!(all(&d).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Item 1: a simulated entry is refused even though none is written.
    #[test]
    fn a_simulated_entry_is_marked_so_the_engine_drops_it() {
        let d = dir("sim");
        let mut e = entry();
        e.as_object_mut()
            .unwrap()
            .insert("simulated".into(), json!(true));
        write(&d, &file(json!([e])));
        assert!(all(&d).unwrap()[0].saved.simulated);
        let _ = std::fs::remove_dir_all(&d);
    }
}
