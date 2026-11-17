//! The state engine (SPEC.md §8.4; ADR 0010, 0016, 0018, 0019, 0021). One `step`
//! per input; every effect it returns belongs to that input's time.
//!
//! Inputs the app provides instead of the engine reading them: the clock
//! (`Tick`), whether notifications are suppressed (`SetSuppressed`: pause or
//! quiet hours, worked out by the app from local time; sent before the first
//! event and on every change), process liveness (`ProcessGone`) and the user
//! actions (`user.ack`, `user.dismiss`).
//!
//! Requesters (ADR 0019): the agent that asked, `None` for the main agent or
//! `Some(agentId)` for a sub-agent. An episode holds every requester still
//! waiting and closes when none is left. If the main agent's turn already
//! completed (ADR 0021), it closes to `ready`, not `working`.
//!
//! Known process (ADR 0018 item 7): an event's `context.pid` is set by the
//! app exactly when it found the session's process (§8.3, §9.4). A session
//! none of whose events carried a pid has no known process, so it counts as
//! having no live process and is removed 6 h after its last event. Once a pid
//! arrives, only `ProcessGone` starts that clock. Simulated sessions never get
//! a pid (§8.1), so they follow the no-process rule.
//!
//! A (state, input) pair with no §8.4 row leaves the session unchanged.
//!
//! Restore (FR-13, ADR 0037): [`Engine::restore`] puts back the sessions the
//! app saved before a restart, with each recorded process re-checked by the
//! app ([`Liveness`]). It emits no effects. A restored session's clocks
//! (escalation, stall, the 60 s fade) count from the restore time, while its
//! `since` and the 6-hour rule's last-event time stay as saved. A restored
//! episode doesn't know who was waiting (no agent ids are saved), so it waits
//! on one unknown requester: the first activity from an agent that isn't a
//! known waiting requester, or the main agent's Stop, stands for it.

use crate::adapter::{is_id, project_key};
use crate::event::{Category, Event, Kind};
use std::collections::HashMap;

const DEDUP_MS: u64 = 10_000;
const FADE_MS: u64 = 60_000;
const REMOVE_MS: u64 = 6 * 3_600_000;

/// The v1 animal ids, in assignment order (§8.7, ADR 0030).
pub const ANIMALS: [&str; 4] = ["fox", "rabbit", "cat", "owl"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// §8.6 `stallMinutes`, in ms.
    pub stall_ms: u64,
    /// §8.6 `escalationMinutes`, in ms.
    pub escalation_ms: u64,
    pub notify_on_stall: bool,
    /// Animal ids in assignment order; [`ANIMALS`] by default.
    pub animals: Vec<String>,
    /// §8.6 `animalOverrides`: [`project_key`] → animal id.
    pub animal_overrides: HashMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            stall_ms: 10 * 60_000,
            escalation_ms: 5 * 60_000,
            notify_on_stall: false,
            animals: ANIMALS.map(String::from).to_vec(),
            animal_overrides: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// `simulated`: the request carried `X-Knock-Sim: 1` (§8.1).
    Event { event: Event, simulated: bool },
    /// Once a second.
    Tick { now: u64 },
    /// `pauseNotifications` is on or it is quiet hours. Holds from this input
    /// on, until the next one; starts `false`. The app sends it before the
    /// first event and whenever the value changes, never waiting for a tick.
    SetSuppressed(bool),
    /// The session's recorded process is no longer alive (§9.7).
    ProcessGone { session_id: String, now: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Notify {
        session_id: String,
        reason: Reason,
    },
    Escalate {
        session_id: String,
    },
    ReadyBatchAdd {
        session_id: String,
    },
    /// The snapshot (sessions, states, episodes) changed; save it.
    Persist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NeedsPermission,
    NeedsAnswer,
    Failed,
    Stalled,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsPermission => "needs_permission",
            Self::NeedsAnswer => "needs_answer",
            Self::Failed => "failed",
            Self::Stalled => "stalled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NeedsInput,
    Failed,
    Stalled,
    /// Unused in v1.
    BlockedFixable,
    Ready,
    Working,
    /// Unused in v1.
    WaitingAgent,
    /// Unused in v1.
    Communicating,
    Idle,
    EndedUnexpectedly,
    /// Unused in v1.
    Paused,
    /// Unused in v1.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    NeedsYou,
    Ready,
    Working,
    Resting,
}

impl State {
    pub fn tier(self) -> Tier {
        use State::*;
        match self {
            NeedsInput | Failed | Stalled | BlockedFixable => Tier::NeedsYou,
            Ready => Tier::Ready,
            Working | WaitingAgent | Communicating => Tier::Working,
            Idle | EndedUnexpectedly | Paused | Cancelled => Tier::Resting,
        }
    }
    pub fn as_str(self) -> &'static str {
        use State::*;
        match self {
            NeedsInput => "needs_input",
            Failed => "failed",
            Stalled => "stalled",
            BlockedFixable => "blocked_fixable",
            Ready => "ready",
            Working => "working",
            WaitingAgent => "waiting_agent",
            Communicating => "communicating",
            Idle => "idle",
            EndedUnexpectedly => "ended_unexpectedly",
            Paused => "paused",
            Cancelled => "cancelled",
        }
    }

    /// The state named `s` ([`State::as_str`]), if any.
    pub fn parse(s: &str) -> Option<Self> {
        use State::*;
        [
            NeedsInput,
            Failed,
            Stalled,
            BlockedFixable,
            Ready,
            Working,
            WaitingAgent,
            Communicating,
            Idle,
            EndedUnexpectedly,
            Paused,
            Cancelled,
        ]
        .into_iter()
        .find(|st| st.as_str() == s)
    }

    /// v1 has an event or timer that produces it (§8.4 table).
    fn produced_in_v1(self) -> bool {
        use State::*;
        matches!(
            self,
            NeedsInput | Failed | Stalled | Ready | Working | Idle | EndedUnexpectedly
        )
    }
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs_you",
            Self::Ready => "ready",
            Self::Working => "working",
            Self::Resting => "resting",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedsKind {
    Permission,
    Answer,
}

/// The open `needs_input` episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needs {
    /// Set by the event that opened the episode; never changes (ADR 0010).
    pub kind: NeedsKind,
    /// The tool, for a permission; `None` for a question or when unknown.
    pub label: Option<String>,
    /// The user clicked or jumped to it (ADR 0016).
    pub seen: bool,
    pub opened: u64,
    pub escalated: bool,
    /// The requesters still waiting (ADR 0019). Kept only for the episode.
    /// Never empty unless `unknown` is set.
    waiting: Vec<Option<String>>,
    /// A requester from before a restart, whose agent isn't known, still
    /// waits (ADR 0037 item 5).
    unknown: bool,
}

impl Needs {
    /// The requesters still waiting: `None` is the main agent.
    pub fn waiting(&self) -> &[Option<String>] {
        &self.waiting
    }

    /// A requester from before a restart still waits; which agent it is
    /// isn't known (ADR 0037 item 5).
    pub fn unknown_requester(&self) -> bool {
        self.unknown
    }
}

/// Everything `SessionView` (§8.5) needs except precision, which is app-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub project: String,
    /// [`project_key`] of the first event's `cwd`; the `cwd` itself is not kept (§11.4).
    pub project_key: String,
    pub state: State,
    /// When `state` was entered.
    pub since: u64,
    pub activity: Option<Category>,
    /// `Some` exactly when `state` is `needs_input`.
    pub needs: Option<Needs>,
    pub animal: String,
    pub simulated: bool,
    /// Last agent event (user actions don't count).
    pub last_event: u64,
    gone_at: Option<u64>,
    /// Some event carried a pid (see the module docs).
    process_known: bool,
    /// Last request-type `needs.*` per requester, for the dedup; only the
    /// last 10 s are kept.
    requests: Vec<(Option<String>, u64)>,
    /// The main agent's turn completed and it hasn't asked or worked since
    /// (ADR 0021).
    turn_done: bool,
    /// The escalation, stall and fade clocks count from no earlier than
    /// this: the restore time for a restored session, else 0 (ADR 0037 item 4).
    clock_from: u64,
}

impl Session {
    /// The main agent's turn completed and it hasn't asked or worked since
    /// (ADR 0021); saved so a restored episode can close to `ready`.
    pub fn turn_done(&self) -> bool {
        self.turn_done
    }

    /// A session as its first event creates it (also used to restart an
    /// `ended_unexpectedly` one, ADR 0018).
    fn new(e: &Event, simulated: bool, animal: String) -> Self {
        Session {
            id: e.session_id.clone(),
            project: e.context.project.clone(),
            project_key: project_key(&e.context.cwd),
            state: State::Idle,
            since: e.ts,
            activity: None,
            needs: None,
            animal,
            simulated,
            last_event: e.ts,
            gone_at: None,
            process_known: e.context.pid.is_some(),
            requests: Vec::new(),
            turn_done: false,
            clock_from: 0,
        }
    }

    fn set(&mut self, to: State, now: u64) {
        if self.state != to {
            self.state = to;
            self.since = now;
        }
        if to != State::NeedsInput {
            self.needs = None;
        }
    }

    /// §8.4 "Process gone without `session.ended`": `ended_unexpectedly`,
    /// except that `failed` stays `failed`.
    fn gone(&mut self, now: u64) {
        self.gone_at.get_or_insert(now);
        if self.state != State::Failed {
            self.set(State::EndedUnexpectedly, now);
        }
    }

    /// Removed by the next tick at `now`: faded 60 s after it ended, or no
    /// events and no live process for 6 h (ADR 0018).
    fn expired(&self, now: u64) -> bool {
        let ended = now.saturating_sub(self.since.max(self.clock_from));
        let faded = self.state == State::EndedUnexpectedly && ended >= FADE_MS;
        // No live process: reported gone, or never known (ADR 0018).
        let gone = self.gone_at.or((!self.process_known).then_some(0));
        let quiet = gone.is_some_and(|g| now.saturating_sub(g.max(self.last_event)) >= REMOVE_MS);
        faded || quiet
    }
}

/// Whether a saved session's recorded process still runs, as the app
/// re-checked it at start (§9.7: same PID and same start time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// No process was recorded (ADR 0018 item 7, ADR 0037 item 6).
    Unknown,
    Alive,
    /// Not running, or its PID now belongs to another process.
    Gone,
}

/// A saved `needs_input` episode (§11.4 data only: no requesters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedNeeds {
    pub kind: NeedsKind,
    pub label: Option<String>,
    pub seen: bool,
    pub opened: u64,
    pub escalated: bool,
}

/// One session as the app saved it and read it back (FR-13, ADR 0037).
/// Holds no agent ids, no cwd and no free text (§11.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub id: String,
    pub project: String,
    pub project_key: String,
    pub state: State,
    pub since: u64,
    pub last_event: u64,
    pub activity: Option<Category>,
    pub needs: Option<SavedNeeds>,
    pub animal: String,
    /// [`Session::turn_done`]; `false` when the file doesn't say.
    pub turn_done: bool,
    /// Never restored (§8.1, ADR 0037 item 1).
    pub simulated: bool,
    pub process: Liveness,
}

impl Saved {
    /// What the app saves of `s`, with its re-checked `process`.
    pub fn of(s: &Session, process: Liveness) -> Self {
        Saved {
            id: s.id.clone(),
            project: s.project.clone(),
            project_key: s.project_key.clone(),
            state: s.state,
            since: s.since,
            last_event: s.last_event,
            activity: s.activity,
            needs: s.needs.as_ref().map(|n| SavedNeeds {
                kind: n.kind,
                label: n.label.clone(),
                seen: n.seen,
                opened: n.opened,
                escalated: n.escalated,
            }),
            animal: s.animal.clone(),
            turn_done: s.turn_done,
            simulated: s.simulated,
            process,
        }
    }
}

/// Why [`Engine::restore`] refused a snapshot. Kinds only, never content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreError {
    /// The engine already has sessions: restore runs once, at start.
    NotEmpty,
    /// A session id the adapter would not accept (§8.3).
    BadId,
    DuplicateId,
    /// A state marked "unused in v1" (§8.4).
    UnusedState,
    /// `needs_input` without its episode.
    NeedsMissing,
    /// An episode on a state other than `needs_input`.
    NeedsUnexpected,
}

#[derive(Debug, Clone, Default)]
pub struct Engine {
    settings: Settings,
    suppressed: bool,
    sessions: Vec<Session>,
}

type Snapshot = Vec<(String, State, Option<Needs>)>;

impl Engine {
    pub fn new(settings: Settings) -> Self {
        Engine {
            settings,
            ..Self::default()
        }
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// In the order they first appeared.
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn step(&mut self, input: Input) -> Vec<Effect> {
        let before = self.snapshot();
        let mut fx = Vec::new();
        match input {
            Input::Event { event, simulated } => self.event(event, simulated, &mut fx),
            Input::Tick { now } => self.tick(now, &mut fx),
            Input::SetSuppressed(on) => self.suppressed = on,
            Input::ProcessGone { session_id, now } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session_id) {
                    s.gone(now);
                }
            }
        }
        if self.snapshot() != before {
            fx.push(Effect::Persist);
        }
        fx
    }

    /// FR-13, ADR 0037: puts back the saved sessions at start, at time `now`,
    /// in their saved order. Emits no effects (item 8) and starts with no
    /// dedup memory. All or nothing: on an error no session is restored.
    ///
    /// - Simulated sessions are skipped (item 1).
    /// - Each comes back in its saved state, with its animal and `since`; an
    ///   episode keeps its kind, label, `seen` and `escalated` (item 2) and
    ///   waits on one unknown requester (item 5).
    /// - [`Liveness::Gone`] follows the "Process gone" row at `now`: the 60 s
    ///   fade counts from the restart; `failed` stays `failed` (item 3).
    /// - The escalation and stall clocks count from `now`; the 6-hour rule
    ///   keeps the saved last-event time (item 4), so a session it already
    ///   removes is not restored.
    ///
    /// Returns how many sessions were restored.
    pub fn restore(&mut self, saved: Vec<Saved>, now: u64) -> Result<usize, RestoreError> {
        if !self.sessions.is_empty() {
            return Err(RestoreError::NotEmpty);
        }
        let saved: Vec<Saved> = saved.into_iter().filter(|s| !s.simulated).collect();
        for (i, s) in saved.iter().enumerate() {
            if !is_id(&s.id) {
                return Err(RestoreError::BadId);
            }
            if saved[..i].iter().any(|o| o.id == s.id) {
                return Err(RestoreError::DuplicateId);
            }
            if !s.state.produced_in_v1() {
                return Err(RestoreError::UnusedState);
            }
            match (s.state == State::NeedsInput, s.needs.is_some()) {
                (true, false) => return Err(RestoreError::NeedsMissing),
                (false, true) => return Err(RestoreError::NeedsUnexpected),
                _ => {}
            }
        }
        for s in saved {
            let mut session = Session {
                id: s.id,
                project: s.project,
                project_key: s.project_key,
                state: s.state,
                since: s.since,
                activity: s.activity,
                needs: s.needs.map(|n| Needs {
                    kind: n.kind,
                    // As `open` sets it: a tool only for a permission.
                    label: n.label.filter(|_| n.kind == NeedsKind::Permission),
                    seen: n.seen,
                    opened: n.opened,
                    escalated: n.escalated,
                    waiting: Vec::new(),
                    unknown: true,
                }),
                animal: s.animal,
                simulated: false,
                last_event: s.last_event,
                gone_at: None,
                process_known: s.process != Liveness::Unknown,
                requests: Vec::new(),
                turn_done: s.turn_done,
                clock_from: now,
            };
            if s.process == Liveness::Gone {
                session.gone(now);
            }
            if !session.expired(now) {
                self.sessions.push(session);
            }
        }
        Ok(self.sessions.len())
    }

    fn snapshot(&self) -> Snapshot {
        let s = self.sessions.iter();
        s.map(|s| (s.id.clone(), s.state, s.needs.clone()))
            .collect()
    }

    fn notify(&self, fx: &mut Vec<Effect>, id: &str, reason: Reason) {
        if !self.suppressed {
            fx.push(Effect::Notify {
                session_id: id.into(),
                reason,
            });
        }
    }

    fn animal_for(&self, key: &str) -> String {
        if let Some(a) = self.settings.animal_overrides.get(key) {
            return a.clone();
        }
        let all = &self.settings.animals;
        let free = all
            .iter()
            .find(|a| self.sessions.iter().all(|s| &s.animal != *a));
        free.or_else(|| all.get(self.sessions.len() % all.len().max(1)))
            .cloned()
            .unwrap_or_default()
    }

    fn event(&mut self, e: Event, simulated: bool, fx: &mut Vec<Effect>) {
        let (ts, kind) = (e.ts, e.kind);
        let user = matches!(kind, Kind::UserAck | Kind::UserDismiss);
        let restart = !user && kind != Kind::SessionEnded;
        let i = match self.sessions.iter().position(|s| s.id == e.session_id) {
            // ADR 0018: an agent event continues an ended_unexpectedly
            // session (e.g. `--resume` in a new terminal) as a first event,
            // keeping its animal and its place.
            Some(i) if restart && self.sessions[i].state == State::EndedUnexpectedly => {
                let animal = std::mem::take(&mut self.sessions[i].animal);
                self.sessions[i] = Session::new(&e, simulated, animal);
                return self.first(i, e, fx);
            }
            Some(i) => i,
            None if !restart => return,
            None => {
                let animal = self.animal_for(&project_key(&e.context.cwd));
                self.sessions.push(Session::new(&e, simulated, animal));
                return self.first(self.sessions.len() - 1, e, fx);
            }
        };
        let suppressed = self.suppressed;
        let s = &mut self.sessions[i];
        if !user {
            s.last_event = ts;
            s.process_known |= e.context.pid.is_some();
        }
        use State::*;
        match (kind, s.state) {
            (Kind::TurnStarted, Idle | Ready | NeedsInput | Stalled | Failed) => {
                s.activity = None;
                s.turn_done = false;
                s.set(Working, ts);
            }
            // ADR 0019: background activity leaves Failed as it is.
            (Kind::Activity, Failed) if e.context.agent_id.is_some() => {}
            (Kind::Activity, Working | Stalled | Failed) => {
                s.turn_done &= e.context.agent_id.is_some();
                s.activity = e.detail.category.or(s.activity);
                s.set(Working, ts);
            }
            // Removes that requester only; closes when none is left (ADR
            // 0019), to ready if the main agent's turn is done (ADR 0021).
            (Kind::Activity, NeedsInput) => {
                if remove(s, &e.context.agent_id) == 0 {
                    if s.turn_done {
                        s.set(Ready, ts);
                        fx.push(Effect::ReadyBatchAdd {
                            session_id: s.id.clone(),
                        });
                    } else {
                        s.activity = e.detail.category.or(s.activity);
                        s.set(Working, ts);
                    }
                }
            }
            (Kind::NeedsPermission | Kind::NeedsAnswer, from) => {
                let dup = dedup(s, &e);
                let who = &e.context.agent_id;
                // The main agent asking again: its turn isn't done (ADR 0021).
                s.turn_done &= dup || who.is_some();
                // `Some((waiting, unknown))`: open a new episode that also
                // waits on them.
                let opens = match (from, &mut s.needs) {
                    _ if dup => None,
                    (Working | Idle | Ready | Stalled | Failed, _) => Some((Vec::new(), false)),
                    // ADR 0018 item 1: a new request (never the
                    // `permission_prompt` Notification) after the user saw
                    // the episode is a new one; the old requesters still
                    // wait, an unknown one from before a restart included.
                    (NeedsInput, Some(n)) if n.seen && !is_prompt(&e) => {
                        Some((std::mem::take(&mut n.waiting), n.unknown))
                    }
                    // Otherwise the same episode; the requester is added.
                    (NeedsInput, Some(n)) => {
                        if !n.waiting.contains(who) {
                            n.waiting.push(who.clone());
                        }
                        None
                    }
                    _ => None,
                };
                if let Some((waiting, unknown)) = opens {
                    self.open(i, e, waiting, unknown, fx);
                }
            }
            // The Stop removes the main agent only (ADR 0019).
            (Kind::TurnCompleted, NeedsInput | Working | Stalled) => {
                s.turn_done = true;
                if remove(s, &None) == 0 {
                    s.set(Ready, ts);
                    fx.push(Effect::ReadyBatchAdd {
                        session_id: s.id.clone(),
                    });
                }
            }
            (Kind::AgentFailed, from) => {
                s.set(Failed, ts);
                if from != Failed && !suppressed {
                    fx.push(Effect::Notify {
                        session_id: s.id.clone(),
                        reason: Reason::Failed,
                    });
                }
            }
            (Kind::SessionEnded, _) | (Kind::UserDismiss, Failed | EndedUnexpectedly) => {
                self.sessions.remove(i);
            }
            (Kind::UserAck, NeedsInput) => {
                if let Some(n) = &mut s.needs {
                    n.seen = true;
                }
            }
            (Kind::UserAck, Ready) => s.set(Idle, ts),
            _ => {}
        }
    }

    /// The first event of session `i`: `idle`, or the state the event implies.
    fn first(&mut self, i: usize, e: Event, fx: &mut Vec<Effect>) {
        let ts = e.ts;
        match e.kind {
            Kind::TurnStarted => self.sessions[i].set(State::Working, ts),
            Kind::Activity => {
                self.sessions[i].activity = e.detail.category;
                self.sessions[i].set(State::Working, ts);
            }
            Kind::NeedsPermission | Kind::NeedsAnswer => {
                dedup(&mut self.sessions[i], &e);
                self.open(i, e, Vec::new(), false, fx);
            }
            Kind::TurnCompleted => {
                self.sessions[i].turn_done = true;
                self.sessions[i].set(State::Ready, ts);
                fx.push(Effect::ReadyBatchAdd {
                    session_id: e.session_id,
                });
            }
            Kind::AgentFailed => {
                self.sessions[i].set(State::Failed, ts);
                self.notify(fx, &e.session_id, Reason::Failed);
            }
            _ => {}
        }
    }

    /// New `needs_input` episode, with its one `Notify`, waiting on
    /// `waiting`, the event's requester and, if `unknown`, an unknown one.
    fn open(
        &mut self,
        i: usize,
        e: Event,
        mut waiting: Vec<Option<String>>,
        unknown: bool,
        fx: &mut Vec<Effect>,
    ) {
        if !waiting.contains(&e.context.agent_id) {
            waiting.push(e.context.agent_id.clone());
        }
        let (kind, reason) = match e.kind {
            Kind::NeedsAnswer => (NeedsKind::Answer, Reason::NeedsAnswer),
            _ => (NeedsKind::Permission, Reason::NeedsPermission),
        };
        let s = &mut self.sessions[i];
        s.set(State::NeedsInput, e.ts);
        s.needs = Some(Needs {
            kind,
            label: e.detail.tool.filter(|_| kind == NeedsKind::Permission),
            seen: false,
            opened: e.ts,
            escalated: false,
            waiting,
            unknown,
        });
        self.notify(fx, &e.session_id, reason);
    }

    fn tick(&mut self, now: u64, fx: &mut Vec<Effect>) {
        let (stall, escalation) = (self.settings.stall_ms, self.settings.escalation_ms);
        let (suppressed, notify_stall) = (self.suppressed, self.settings.notify_on_stall);
        self.sessions.retain(|s| !s.expired(now));
        for s in &mut self.sessions {
            // ADR 0037 item 4: no clock counts time before a restore.
            let from = s.clock_from;
            match s.state {
                State::Working if now.saturating_sub(s.last_event.max(from)) >= stall => {
                    s.set(State::Stalled, now);
                    if notify_stall && !suppressed {
                        fx.push(Effect::Notify {
                            session_id: s.id.clone(),
                            reason: Reason::Stalled,
                        });
                    }
                }
                State::NeedsInput => {
                    let Some(n) = &mut s.needs else { continue };
                    let open_for = now.saturating_sub(n.opened.max(from));
                    if !n.escalated && !n.seen && open_for >= escalation {
                        // Suppressed means dropped, not deferred.
                        n.escalated = true;
                        if !suppressed {
                            fx.push(Effect::Escalate {
                                session_id: s.id.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The 10 s dedup (§8.4, ADR 0010; per requester, ADR 0019): a
/// `permission_prompt` Notification (the `needs.permission` with no tool)
/// within 10 s after a PermissionRequest from the same requester belongs to
/// that request's episode, even if it closed. A prompt with no `agentId`
/// matches any requester's request (ADR 0021). Every other `needs.*` counts as
/// the request half and is never a duplicate itself, so a second request
/// right after a prompt still knocks.
fn dedup(s: &mut Session, e: &Event) -> bool {
    let who = &e.context.agent_id;
    s.requests
        .retain(|(_, t)| e.ts.saturating_sub(*t) <= DEDUP_MS);
    if is_prompt(e) {
        s.requests.iter().any(|(a, _)| who.is_none() || a == who)
    } else {
        s.requests.retain(|(a, _)| a != who);
        s.requests.push((who.clone(), e.ts));
        false
    }
}

/// Removes `who` from the open episode's requesters; returns how many are
/// left (0 when no episode is open). An agent that isn't a known waiting
/// requester stands for the unknown one from before a restart, if any
/// (ADR 0037 item 5).
fn remove(s: &mut Session, who: &Option<String>) -> usize {
    s.needs.as_mut().map_or(0, |n| {
        if n.waiting.contains(who) {
            n.waiting.retain(|a| a != who);
        } else {
            n.unknown = false;
        }
        n.waiting.len() + usize::from(n.unknown)
    })
}

/// The `permission_prompt` Notification: the `needs.permission` with no tool.
fn is_prompt(e: &Event) -> bool {
    e.kind == Kind::NeedsPermission && e.detail.tool.is_none()
}
