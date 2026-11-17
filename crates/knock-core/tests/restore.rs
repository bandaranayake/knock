//! §12.1 Engine: restoring sessions after a restart (FR-13, §8.4 "Restore",
//! ADR 0037 items 1-8), and the §8.4 rows applied to restored sessions.

use knock_core::{
    Category, Context, Detail, Effect, Engine, Event, Input, Kind, Liveness, NeedsKind, Reason,
    RestoreError, Saved, SavedNeeds, Session, Settings, State,
};

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
/// §8.4: an ended session fades after 60 s.
const FADE: u64 = 60_000;
/// When Knock closed (the last save).
const T0: u64 = 1_792_166_400_000;
/// When Knock started again.
const R: u64 = T0 + 30 * MIN;

fn ev(kind: Kind, ts: u64) -> Event {
    Event {
        v: 1,
        agent: "claude-code",
        session_id: "s1".into(),
        ts,
        kind,
        detail: Detail::default(),
        context: Context {
            cwd: r"C:\code\demo".into(),
            project: "demo".into(),
            pid: None,
            agent_id: None,
        },
    }
}

fn agent(mut e: Event, a: &str) -> Event {
    e.context.agent_id = Some(a.into());
    e
}

/// A PermissionRequest: `needs.permission` with a tool.
fn request(ts: u64) -> Event {
    let mut e = ev(Kind::NeedsPermission, ts);
    e.detail.tool = Some("Write".into());
    e.detail.category = Some(Category::Edit);
    e
}

/// The `permission_prompt` Notification: `needs.permission` with no tool.
fn prompt(ts: u64) -> Event {
    ev(Kind::NeedsPermission, ts)
}

/// A saved session `s1` in `state`, entered at `T0 - 2 min`, last event at
/// `T0 - 1 min`, its process alive.
fn saved(state: State) -> Saved {
    Saved {
        id: "s1".into(),
        project: "demo".into(),
        project_key: "0123456789abcdef".into(),
        state,
        since: T0 - 2 * MIN,
        last_event: T0 - MIN,
        activity: Some(Category::Run),
        needs: (state == State::NeedsInput).then(|| SavedNeeds {
            kind: NeedsKind::Permission,
            label: Some("PowerShell".into()),
            seen: false,
            opened: T0 - 2 * MIN,
            escalated: false,
        }),
        animal: "owl".into(),
        turn_done: false,
        simulated: false,
        process: Liveness::Alive,
    }
}

fn needs(seen: bool, escalated: bool) -> Saved {
    let mut s = saved(State::NeedsInput);
    let n = s.needs.as_mut().unwrap();
    (n.seen, n.escalated) = (seen, escalated);
    s
}

struct H(Engine);

impl H {
    /// A fresh engine at start (default settings), restored at `R`.
    fn restore(saved: Vec<Saved>) -> Self {
        let mut e = Engine::new(Settings::default());
        e.restore(saved, R).expect("restored");
        H(e)
    }
    fn one(s: Saved) -> Self {
        Self::restore(vec![s])
    }
    fn send(&mut self, e: Event) -> Vec<Effect> {
        self.0.step(Input::Event {
            event: e,
            simulated: false,
        })
    }
    fn tick(&mut self, now: u64) -> Vec<Effect> {
        self.0.step(Input::Tick { now })
    }
    fn s1(&self) -> &Session {
        self.0.sessions().iter().find(|s| s.id == "s1").unwrap()
    }
    fn state(&self) -> Option<State> {
        let s = self.0.sessions().iter().find(|s| s.id == "s1");
        s.map(|s| s.state)
    }
    /// The episode's known requesters; `None` is the main agent.
    fn waiting(&self) -> Vec<Option<&str>> {
        let n = self.s1().needs.as_ref().unwrap();
        n.waiting().iter().map(Option::as_deref).collect()
    }
}

fn notify(reason: Reason) -> Effect {
    Effect::Notify {
        session_id: "s1".into(),
        reason,
    }
}
fn escalate() -> Effect {
    Effect::Escalate {
        session_id: "s1".into(),
    }
}
fn ready_add() -> Effect {
    Effect::ReadyBatchAdd {
        session_id: "s1".into(),
    }
}
use Effect::Persist;

/// Effects from ticking every second from `from` to `to`, inclusive.
fn ticks(h: &mut H, from: u64, to: u64) -> Vec<Effect> {
    (from..=to).step_by(1000).flat_map(|t| h.tick(t)).collect()
}

const V1: [State; 7] = [
    State::Idle,
    State::Working,
    State::Ready,
    State::NeedsInput,
    State::Failed,
    State::Stalled,
    State::EndedUnexpectedly,
];

// Item 1: a live session comes back in its saved state, with its animal,
// project and time in state.
#[test]
fn item1_a_live_session_comes_back_as_it_was() {
    for state in V1 {
        let h = H::one(saved(state));
        let s = h.s1();
        assert_eq!(Saved::of(s, Liveness::Alive), saved(state), "{state:?}");
        assert_eq!((s.since, s.last_event), (T0 - 2 * MIN, T0 - MIN));
        assert!(!s.simulated);
    }
}

// Item 1: in the saved order; and nothing about a session leaks into another.
#[test]
fn item1_several_sessions_come_back_in_their_saved_order() {
    let mut a = saved(State::Working);
    a.id = "a".into();
    a.animal = "fox".into();
    let mut b = needs(true, false);
    b.id = "b".into();
    b.animal = "rabbit".into();
    let h = H::restore(vec![b.clone(), a.clone()]);
    let got: Vec<_> = (h.0.sessions().iter())
        .map(|s| Saved::of(s, Liveness::Alive))
        .collect();
    assert_eq!(got, [b, a]);
}

// Item 1: simulated sessions are never restored.
#[test]
fn item1_simulated_sessions_are_never_restored() {
    let mut sim = saved(State::NeedsInput);
    sim.simulated = true;
    let mut real = saved(State::Ready);
    real.id = "real".into();
    let mut e = Engine::new(Settings::default());
    assert_eq!(e.restore(vec![sim, real], R), Ok(1));
    let ids: Vec<_> = e.sessions().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["real"]);
}

// A new session after a restore gets an animal no restored session has.
#[test]
fn item1_a_new_session_after_a_restore_gets_a_free_animal() {
    let mut a = saved(State::Ready);
    a.animal = "fox".into();
    let mut h = H::one(a);
    let mut e = ev(Kind::TurnStarted, R + 1000);
    e.session_id = "new".into();
    h.send(e);
    assert_eq!(h.0.sessions()[1].animal, "rabbit");
}

// Item 2: a Needs-you episode is the same episode: kind, label, seen and
// escalated as saved, and no second Notify.
#[test]
fn item2_a_restored_episode_keeps_kind_label_seen_and_escalated() {
    for (seen, escalated) in [(false, false), (true, false), (false, true), (true, true)] {
        let h = H::one(needs(seen, escalated));
        let n = h.s1().needs.clone().unwrap();
        assert_eq!(
            (n.kind, n.label.as_deref(), n.seen, n.opened, n.escalated),
            (
                NeedsKind::Permission,
                Some("PowerShell"),
                seen,
                T0 - 2 * MIN,
                escalated
            )
        );
        assert!(n.waiting().is_empty() && n.unknown_requester());
    }
    // A question keeps no tool label, however the file reads it.
    for label in [None, Some("PowerShell".to_string())] {
        let mut q = needs(false, false);
        q.needs.as_mut().unwrap().kind = NeedsKind::Answer;
        q.needs.as_mut().unwrap().label = label;
        let h = H::one(q);
        let n = h.s1().needs.clone().unwrap();
        assert_eq!((n.kind, n.label), (NeedsKind::Answer, None));
    }
}

// Item 2 + item 8: restoring and the ticks after it send nothing until the
// escalation is due; a seen episode stays calm and never escalates.
#[test]
fn item2_a_seen_restored_episode_stays_calm_and_never_escalates() {
    let mut h = H::one(needs(true, false));
    assert_eq!(ticks(&mut h, R, R + 2 * HOUR), vec![]);
    assert_eq!(h.state(), Some(State::NeedsInput));
    assert!(h.s1().needs.as_ref().unwrap().seen);
}

// Item 3: a session whose process is gone at start ends unexpectedly at the
// restore time and fades 60 s later, without effects.
#[test]
fn item3_a_gone_process_ends_unexpectedly_and_fades_60s_after_the_restart() {
    for state in V1.into_iter().filter(|s| *s != State::Failed) {
        let mut s = saved(state);
        s.process = Liveness::Gone;
        let mut h = H::one(s);
        let s = h.s1();
        // Already ended: it keeps its time in state.
        let since = match state {
            State::EndedUnexpectedly => T0 - 2 * MIN,
            _ => R,
        };
        assert_eq!(
            (s.state, s.since, s.needs.clone()),
            (State::EndedUnexpectedly, since, None),
            "{state:?}"
        );
        assert_eq!(h.tick(R + 59_999), vec![], "{state:?}");
        assert_eq!(h.tick(R + 60_000), vec![Persist], "{state:?}");
        assert_eq!(h.state(), None, "{state:?}");
    }
}

// Item 3: Failed stays Failed until dismissed, or until the 6-hour rule
// removes it counting from the SAVED last event (item 4), not from the
// restart: restarting Knock never gives a stale session another 6 hours.
#[test]
fn item3_failed_stays_failed_when_its_process_is_gone() {
    let mut s = saved(State::Failed);
    s.process = Liveness::Gone;
    let mut h = H::one(s);
    assert_eq!(h.state(), Some(State::Failed));
    assert_eq!(h.s1().since, T0 - 2 * MIN, "time in state kept");
    assert_eq!(ticks(&mut h, R, R + 10 * MIN), vec![]);
    assert_eq!(h.state(), Some(State::Failed));
    // The saved last event was at T0 - 1 min, 31 minutes before the restart.
    let removal = T0 - MIN + 6 * HOUR;
    assert!(removal < R + 6 * HOUR, "not six hours from the restart");
    assert_eq!(h.tick(removal - 1), vec![]);
    assert_eq!(h.tick(removal), vec![Persist]);
    assert_eq!(h.state(), None);
    // Dismiss removes it.
    let mut s = saved(State::Failed);
    s.process = Liveness::Gone;
    let mut h = H::one(s);
    let mut d = ev(Kind::UserDismiss, R + 1000);
    d.context = Context::default();
    assert_eq!(h.send(d), vec![Persist]);
    assert_eq!(h.state(), None);
}

// Item 3: a session already ended when Knock closed still shows "Session
// ended" for 60 s after the restart; its time in state is kept.
#[test]
fn item3_an_already_ended_session_fades_60s_after_the_restart() {
    let mut s = saved(State::EndedUnexpectedly);
    s.process = Liveness::Unknown;
    let mut h = H::one(s);
    assert_eq!(h.s1().since, T0 - 2 * MIN);
    assert_eq!(h.tick(R + 59_999), vec![]);
    assert_eq!(h.tick(R + 60_000), vec![Persist]);
    assert_eq!(h.state(), None);
}

// Item 4: an unseen, not yet escalated episode escalates escalationMinutes
// after the restore, not after it opened; `since` is unchanged.
#[test]
fn item4_escalation_counts_from_the_restore_time() {
    let mut h = H::one(needs(false, false));
    // It opened 32 minutes before the restore: no catch-up escalation.
    assert_eq!(ticks(&mut h, R, R + 5 * MIN - 1000), vec![]);
    assert_eq!(h.tick(R + 5 * MIN), vec![escalate(), Persist]);
    assert_eq!(h.s1().since, T0 - 2 * MIN);
    assert_eq!(h.s1().needs.as_ref().unwrap().opened, T0 - 2 * MIN);
    // Once per episode.
    assert_eq!(ticks(&mut h, R + 5 * MIN + 1000, R + HOUR), vec![]);
}

// Item 4: an episode that already escalated never escalates again.
#[test]
fn item4_an_already_escalated_episode_never_escalates_again() {
    let mut h = H::one(needs(false, true));
    assert_eq!(ticks(&mut h, R, R + 2 * HOUR), vec![]);
    assert_eq!(h.state(), Some(State::NeedsInput));
}

// Item 4: suppression still drops the restored episode's escalation.
#[test]
fn item4_a_suppressed_restored_escalation_is_dropped() {
    let mut h = H::one(needs(false, false));
    h.0.step(Input::SetSuppressed(true));
    assert_eq!(h.tick(R + 5 * MIN), vec![Persist]);
    h.0.step(Input::SetSuppressed(false));
    assert_eq!(ticks(&mut h, R + 5 * MIN, R + HOUR), vec![]);
}

// Item 4: Working stalls only stallMinutes after the restore with no
// events; `since` stays as saved until the stall.
#[test]
fn item4_stall_counts_from_the_restore_time() {
    let mut h = H::one(saved(State::Working));
    assert_eq!(ticks(&mut h, R, R + 10 * MIN - 1000), vec![]);
    assert_eq!(h.s1().since, T0 - 2 * MIN);
    assert_eq!(h.tick(R + 10 * MIN), vec![Persist]);
    assert_eq!(
        (h.state(), h.s1().since),
        (Some(State::Stalled), R + 10 * MIN)
    );
    // An event after the restore restarts it from that event, as before.
    let mut h = H::one(saved(State::Working));
    h.send(ev(Kind::Activity, R + 3 * MIN));
    assert_eq!(h.tick(R + 10 * MIN), vec![]);
    assert_eq!(h.tick(R + 13 * MIN), vec![Persist]);
    assert_eq!(h.state(), Some(State::Stalled));
    // With stall notifications on, the stall notifies, once.
    let mut e = Engine::new(Settings {
        notify_on_stall: true,
        ..Settings::default()
    });
    e.restore(vec![saved(State::Working)], R).unwrap();
    let mut h = H(e);
    assert_eq!(h.tick(R + 10 * MIN), vec![notify(Reason::Stalled), Persist]);
}

// Item 4: a restored stalled session stays stalled and sends nothing.
#[test]
fn item4_a_restored_stall_sends_nothing() {
    let mut s = saved(State::Stalled);
    s.since = T0 - 20 * MIN;
    let mut h = H::one(s);
    assert_eq!(ticks(&mut h, R, R + HOUR), vec![]);
    assert_eq!(
        (h.state(), h.s1().since),
        (Some(State::Stalled), T0 - 20 * MIN)
    );
}

// Item 4 + item 6: the 6-hour rule uses the saved last-event time.
#[test]
fn item4_the_six_hour_rule_uses_the_saved_last_event_time() {
    let mut s = saved(State::Ready);
    s.process = Liveness::Unknown;
    let mut h = H::one(s);
    let removal = T0 - MIN + 6 * HOUR;
    assert_eq!(h.tick(removal - 1), vec![]);
    assert_eq!(h.tick(removal), vec![Persist]);
    assert_eq!(h.state(), None);
    // Already past it at the restore: not restored at all.
    let mut s = saved(State::NeedsInput);
    s.process = Liveness::Unknown;
    s.last_event = R - 6 * HOUR;
    let mut e = Engine::new(Settings::default());
    assert_eq!(e.restore(vec![s], R), Ok(0));
    assert!(e.sessions().is_empty());
    // A live process keeps it past six hours, as before.
    let mut h = H::one(saved(State::Ready));
    assert_eq!(h.tick(R + 7 * HOUR), vec![]);
    assert_eq!(h.state(), Some(State::Ready));
    // And once that process goes, the clock counts from then.
    h.0.step(Input::ProcessGone {
        session_id: "s1".into(),
        now: R + 7 * HOUR,
    });
    assert_eq!(h.state(), Some(State::EndedUnexpectedly));
    // From there the 60 s fade removes it, well before any 6-hour clock.
    assert_eq!(h.tick(R + 7 * HOUR + FADE - 1), vec![]);
    assert_eq!(h.tick(R + 7 * HOUR + FADE), vec![Persist]);
    assert_eq!(h.state(), None);
}

// Item 4: a session whose process is found gone keeps the 6-hour clock of
// its saved last event, so restarting Knock over and over never keeps a
// stale session alive. (Found by the fresh review of f6881dc.)
#[test]
fn item4_restarting_does_not_restart_the_six_hour_clock() {
    let removal = T0 - MIN + 6 * HOUR;
    // Failed at T0 - 2 min, terminal closed, Knock restarted every hour.
    let mut at = R;
    while at < removal {
        let mut s = saved(State::Failed);
        s.process = Liveness::Gone;
        let mut e = Engine::new(Settings::default());
        assert_eq!(e.restore(vec![s.clone()], at), Ok(1), "restart at {at}");
        let mut h = H(e);
        assert_eq!(h.state(), Some(State::Failed));
        // The saved last event never moves, so neither does the removal.
        assert_eq!(h.tick(removal - 1), vec![]);
        assert_eq!(h.tick(removal), vec![Persist], "restart at {at}");
        assert_eq!(h.state(), None);
        at += HOUR;
    }
    // A restart at or after the removal time doesn't restore it at all.
    let mut s = saved(State::Failed);
    s.process = Liveness::Gone;
    let mut e = Engine::new(Settings::default());
    assert_eq!(e.restore(vec![s], removal), Ok(0));
    assert!(e.sessions().is_empty());
}

/// §6 Reliability (zero missed Needs-you), the failure case the fresh
/// review of f6881dc found: a sub-agent asked for permission, so the session
/// is `needs_input` waiting on it while the main agent works. Knock
/// restarts. The main agent's turn then finishes. The sub-agent is blocked
/// and sends nothing more, so if the Stop closed the episode the knock would
/// be lost for good and the strip would show a calm "ready".
#[test]
fn a_subagents_knock_survives_a_restart_and_the_main_agents_stop() {
    // Before the restart: a sub-agent's request, the main agent working.
    let mut e = Engine::new(Settings::default());
    let send = |e: &mut Engine, ev: Event| {
        e.step(Input::Event {
            event: ev,
            simulated: false,
        })
    };
    send(&mut e, ev(Kind::TurnStarted, T0 - 5 * MIN));
    send(&mut e, agent(request(T0 - 2 * MIN), "sub"));
    assert_eq!(e.sessions()[0].state, State::NeedsInput);
    // Saved, Knock closes, the process is still alive at the restart.
    let snap: Vec<_> = (e.sessions().iter())
        .map(|s| Saved::of(s, Liveness::Alive))
        .collect();
    let mut e = Engine::new(Settings::default());
    e.restore(snap, R).unwrap();
    let mut h = H(e);
    // The main agent finishes. The knock stays: the waiting requester is
    // the sub-agent, which the Stop knows nothing about.
    let fx = h.send(ev(Kind::TurnCompleted, R + 1000));
    assert!(
        !fx.contains(&ready_add()),
        "a sub-agent's knock was lost: {fx:?}"
    );
    assert_eq!(h.state(), Some(State::NeedsInput));
    let n = h.s1().needs.clone().unwrap();
    assert_eq!(
        (n.kind, n.label.as_deref(), n.seen),
        (NeedsKind::Permission, Some("Write"), false)
    );
    // The user can still clear it: answering it produces activity, which
    // closes the episode to ready because the main turn is done (ADR 0021).
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 2000), "sub")),
        vec![ready_add(), Persist]
    );
    assert_eq!(h.state(), Some(State::Ready));
}

/// The accepted consequence of the fix: a restored episode whose session
/// finishes its turn with no activity at all keeps knocking. A click still
/// calms it (ADR 0016) and it never escalates afterwards.
#[test]
fn a_lingering_restored_knock_can_be_cleared_by_the_user() {
    let mut h = H::one(needs(false, false));
    h.send(ev(Kind::TurnCompleted, R + 1000));
    assert_eq!(h.state(), Some(State::NeedsInput));
    // It escalates once, on its restored clock, then never again.
    assert_eq!(h.tick(R + 5 * MIN), vec![escalate(), Persist]);
    let mut ack = ev(Kind::UserAck, R + 6 * MIN);
    ack.context = Context::default();
    assert_eq!(h.send(ack), vec![Persist]);
    assert!(h.s1().needs.as_ref().unwrap().seen, "calm after a click");
    assert_eq!(ticks(&mut h, R + 6 * MIN, R + 3 * HOUR), vec![]);
    assert_eq!(h.state(), Some(State::NeedsInput));
    // And these still close it, as §8.4 says.
    for close in [Kind::TurnStarted, Kind::AgentFailed, Kind::SessionEnded] {
        let mut h = H::one(needs(false, false));
        h.send(ev(Kind::TurnCompleted, R + 1000));
        h.send(ev(close, R + 2000));
        assert_ne!(h.state(), Some(State::NeedsInput), "{close:?}");
    }
}

// Item 5: the first activity, from the main agent or any sub-agent, closes
// a restored episode, as if the last waiting requester had resumed.
#[test]
fn item5_the_first_activity_from_any_agent_closes_a_restored_episode() {
    for who in [None, Some("a1")] {
        for seen in [false, true] {
            let mut h = H::one(needs(seen, false));
            let mut e = ev(Kind::Activity, R + 1000);
            e.detail.category = Some(Category::Read);
            if let Some(a) = who {
                e = agent(e, a);
            }
            assert_eq!(h.send(e), vec![Persist], "{who:?}");
            let s = h.s1();
            assert_eq!(
                (s.state, s.since, s.needs.clone()),
                (State::Working, R + 1000, None)
            );
            assert_eq!(s.activity, Some(Category::Read));
        }
    }
}

// Item 5: the main agent's own request joining a restored episode, then its
// Stop. The Stop removes that request, but the unknown requester from
// before the restart is not it, so the knock stays until some activity
// arrives. (Found by the invariants proptest.)
#[test]
fn item5_a_stop_after_the_main_agent_joined_leaves_the_unknown_requester() {
    let mut h = H::one(needs(false, false));
    // A `permission_prompt` Notification with no tool: the dedup memory is
    // empty after a restart, so it joins the open episode (item 8).
    assert_eq!(h.send(prompt(R + 1000)), vec![Persist]);
    assert_eq!(h.waiting(), [None]);
    // The requester list changed, so the snapshot is saved; no Notify.
    assert_eq!(h.send(ev(Kind::TurnCompleted, R + 2000)), vec![Persist]);
    assert_eq!(h.state(), Some(State::NeedsInput), "no lost knock");
    assert!(h.s1().needs.as_ref().unwrap().unknown_requester());
    // Any activity then closes it, to ready because the turn is done.
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 3000), "a9")),
        vec![ready_add(), Persist]
    );
    assert_eq!(h.state(), Some(State::Ready));
}

// Item 5 + ADR 0021: if the main agent's turn was already done, it closes to
// ready with ReadyBatchAdd. A file without the field means not done.
#[test]
fn item5_a_restored_episode_with_the_turn_done_closes_to_ready() {
    let mut s = needs(false, false);
    s.turn_done = true;
    let mut h = H::one(s);
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 1000), "a1")),
        vec![ready_add(), Persist]
    );
    assert_eq!(h.state(), Some(State::Ready));
    // Stays in ready for later background activity, as before.
    assert_eq!(h.send(agent(ev(Kind::Activity, R + 2000), "a1")), vec![]);
}

// Item 5: a request that joins a restored episode is waited on: the first
// activity from an agent that isn't a known requester stands for the
// unknown one, and the joined requester still has to resume.
#[test]
fn item5_a_request_joining_a_restored_episode_is_not_lost() {
    let mut h = H::one(needs(false, false));
    // Same episode, no Notify (the knock already shows).
    assert_eq!(h.send(agent(request(R + 1000), "a2")), vec![Persist]);
    let n = h.s1().needs.clone().unwrap();
    assert_eq!(
        (n.waiting(), n.unknown_requester()),
        (&[Some("a2".to_string())][..], true)
    );
    // The main agent resumes: the unknown requester is done, a2 still waits.
    assert_eq!(h.send(ev(Kind::Activity, R + 2000)), vec![Persist]);
    assert_eq!(h.state(), Some(State::NeedsInput));
    assert!(!h.s1().needs.as_ref().unwrap().unknown_requester());
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 3000), "a2")),
        vec![Persist]
    );
    assert_eq!(h.state(), Some(State::Working));
    // In the other order: a2 first, then any other agent.
    let mut h = H::one(needs(false, false));
    h.send(agent(request(R + 1000), "a2"));
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 2000), "a2")),
        vec![Persist]
    );
    assert_eq!(h.state(), Some(State::NeedsInput));
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 3000), "a3")),
        vec![Persist]
    );
    assert_eq!(h.state(), Some(State::Working));
}

// Item 5 + ADR 0018 item 1: a new request after a seen restored episode
// opens a new, unseen episode with its Notify; the unknown requester still
// waits in it.
#[test]
fn item5_a_new_request_after_a_seen_restored_episode_knocks() {
    let mut h = H::one(needs(true, false));
    assert_eq!(
        h.send(request(R + 1000)),
        vec![notify(Reason::NeedsPermission), Persist]
    );
    let n = h.s1().needs.clone().unwrap();
    assert_eq!(
        (n.seen, n.opened, n.label.as_deref()),
        (false, R + 1000, Some("Write"))
    );
    assert_eq!((n.waiting(), n.unknown_requester()), (&[None][..], true));
    // It escalates on its own clock, once.
    assert_eq!(h.tick(R + 1000 + 5 * MIN), vec![escalate(), Persist]);
    // The main agent resumes, then any other activity ends the old one.
    h.send(ev(Kind::Activity, R + 6 * MIN));
    assert_eq!(h.state(), Some(State::NeedsInput));
    h.send(agent(ev(Kind::Activity, R + 6 * MIN + 1), "a1"));
    assert_eq!(h.state(), Some(State::Working));
}

// Item 5 + §8.4 ("ready once no sub-agent is waiting"): the main agent's
// Stop removes the main agent only. It never stands for the unknown
// requester, which may be a sub-agent still waiting, so the episode stays
// open whether or not the main agent had joined it.
#[test]
fn item5_turn_completed_on_a_restored_episode() {
    let mut h = H::one(needs(false, false));
    assert_eq!(h.send(ev(Kind::TurnCompleted, R + 1000)), vec![]);
    assert_eq!(h.state(), Some(State::NeedsInput), "no missed knock");
    assert!(h.s1().needs.as_ref().unwrap().unknown_requester());
    assert!(h.s1().turn_done());
    // The turn is done, so the next activity closes it to ready (ADR 0021).
    assert_eq!(
        h.send(ev(Kind::Activity, R + 2000)),
        vec![ready_add(), Persist]
    );
    assert_eq!(
        (h.state(), h.s1().needs.clone()),
        (Some(State::Ready), None)
    );
    // With a sub-agent that joined after the restart, the Stop changes
    // nothing at all: it is not a waiting requester and it no longer
    // consumes the unknown one.
    let mut h = H::one(needs(false, false));
    h.send(agent(request(R + 1000), "a2"));
    assert_eq!(h.send(ev(Kind::TurnCompleted, R + 2000)), vec![]);
    assert_eq!(h.state(), Some(State::NeedsInput));
    assert_eq!(h.waiting(), [Some("a2")]);
    assert!(h.s1().needs.as_ref().unwrap().unknown_requester());
    // a2 answers; the unknown requester still waits.
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 3000), "a2")),
        vec![Persist]
    );
    assert_eq!(h.state(), Some(State::NeedsInput));
    // Any further activity stands for the unknown one and closes it.
    assert_eq!(
        h.send(agent(ev(Kind::Activity, R + 4000), "a3")),
        vec![ready_add(), Persist]
    );
    assert_eq!(h.state(), Some(State::Ready));
}

// Item 5: every other §8.4 row applies to a restored session unchanged.
#[test]
fn item5_the_other_rows_apply_to_a_restored_episode() {
    // turn.started closes it.
    let mut h = H::one(needs(false, false));
    assert_eq!(h.send(ev(Kind::TurnStarted, R + 1000)), vec![Persist]);
    assert_eq!(h.state(), Some(State::Working));
    // user.ack marks it seen; no Escalate afterwards.
    let mut h = H::one(needs(false, false));
    let mut ack = ev(Kind::UserAck, R + 1000);
    ack.context = Context::default();
    assert_eq!(h.send(ack), vec![Persist]);
    assert!(h.s1().needs.as_ref().unwrap().seen);
    assert_eq!(ticks(&mut h, R, R + HOUR), vec![]);
    // agent.failed notifies.
    let mut h = H::one(needs(false, false));
    assert_eq!(
        h.send(ev(Kind::AgentFailed, R + 1000)),
        vec![notify(Reason::Failed), Persist]
    );
    // session.ended removes it.
    let mut h = H::one(needs(false, false));
    assert_eq!(h.send(ev(Kind::SessionEnded, R + 1000)), vec![Persist]);
    assert_eq!(h.state(), None);
    // More needs in an unseen restored episode: same episode, no Notify;
    // the requester is added.
    let mut h = H::one(needs(false, false));
    assert_eq!(h.send(ev(Kind::NeedsAnswer, R + 1000)), vec![Persist]);
    let n = h.s1().needs.clone().unwrap();
    assert_eq!(
        (n.kind, n.opened, n.waiting()),
        (NeedsKind::Permission, T0 - 2 * MIN, &[None][..])
    );
    // The process goes: ended, no effects.
    let mut h = H::one(needs(false, false));
    let gone = Input::ProcessGone {
        session_id: "s1".into(),
        now: R + 1000,
    };
    assert_eq!(h.0.step(gone), vec![Persist]);
    assert_eq!(h.state(), Some(State::EndedUnexpectedly));
}

// Item 5: the other states follow §8.4 as for any session.
#[test]
fn item5_restored_states_follow_the_usual_rows() {
    // ready + user.ack → idle.
    let mut h = H::one(saved(State::Ready));
    let mut ack = ev(Kind::UserAck, R + 1000);
    ack.context = Context::default();
    assert_eq!(h.send(ack), vec![Persist]);
    assert_eq!(h.state(), Some(State::Idle));
    // failed + background activity stays failed (ADR 0019); main clears it.
    let mut h = H::one(saved(State::Failed));
    assert_eq!(h.send(agent(ev(Kind::Activity, R + 1000), "a1")), vec![]);
    assert_eq!(h.send(ev(Kind::Activity, R + 2000)), vec![Persist]);
    assert_eq!(h.state(), Some(State::Working));
    // working + needs → a new episode with Notify.
    let mut h = H::one(saved(State::Working));
    assert_eq!(
        h.send(request(R + 1000)),
        vec![notify(Reason::NeedsPermission), Persist]
    );
    // working + turn.completed → ready + ReadyBatchAdd.
    let mut h = H::one(saved(State::Working));
    assert_eq!(
        h.send(ev(Kind::TurnCompleted, R + 1000)),
        vec![ready_add(), Persist]
    );
    // ended_unexpectedly + an agent event continues it as a first event,
    // keeping its animal.
    let mut s = saved(State::Working);
    s.process = Liveness::Gone;
    let mut h = H::one(s);
    assert_eq!(h.send(ev(Kind::TurnStarted, R + 1000)), vec![Persist]);
    let s = h.s1();
    assert_eq!(
        (s.state, s.since, s.animal.as_str()),
        (State::Working, R + 1000, "owl")
    );
}

// Item 6: a session with no recorded process is restored as it was and
// follows the 6-hour rule from its saved last event.
#[test]
fn item6_a_session_with_no_recorded_process_is_restored_as_it_was() {
    for state in V1 {
        let mut s = saved(state);
        s.process = Liveness::Unknown;
        let h = H::one(s.clone());
        assert_eq!(Saved::of(h.s1(), Liveness::Unknown), s, "{state:?}");
    }
}

// Item 7: an entry that can't be understood refuses the whole snapshot,
// and the engine stays empty.
#[test]
fn item7_an_entry_that_cant_be_understood_refuses_the_whole_snapshot() {
    let good = saved(State::Ready);
    let with = |f: &dyn Fn(&mut Saved)| {
        let mut s = saved(State::Ready);
        s.id = "other".into();
        f(&mut s);
        s
    };
    let unused = [
        State::BlockedFixable,
        State::WaitingAgent,
        State::Communicating,
        State::Paused,
        State::Cancelled,
    ];
    let mut cases: Vec<(Saved, RestoreError)> = unused
        .into_iter()
        .map(|st| {
            (
                with(&|s: &mut Saved| s.state = st),
                RestoreError::UnusedState,
            )
        })
        .collect();
    cases.push((
        with(&|s: &mut Saved| s.state = State::NeedsInput),
        RestoreError::NeedsMissing,
    ));
    cases.push((
        with(&|s: &mut Saved| s.needs = needs(false, false).needs),
        RestoreError::NeedsUnexpected,
    ));
    cases.push((
        with(&|s: &mut Saved| s.id = "s1".into()),
        RestoreError::DuplicateId,
    ));
    let long = "x".repeat(129);
    for bad in ["", "has space", "line\nbreak", long.as_str()] {
        cases.push((
            with(&|s: &mut Saved| s.id = bad.into()),
            RestoreError::BadId,
        ));
    }
    for (bad, why) in cases {
        let mut e = Engine::new(Settings::default());
        assert_eq!(e.restore(vec![good.clone(), bad], R), Err(why));
        assert!(e.sessions().is_empty(), "{why:?}: nothing restored");
    }
    // Restore runs once, on an empty engine.
    let mut h = H::one(good.clone());
    assert_eq!(
        h.0.restore(vec![], R),
        Err(RestoreError::NotEmpty),
        "not twice"
    );
    assert_eq!(h.0.sessions().len(), 1);
    // An empty snapshot restores nothing.
    let mut e = Engine::new(Settings::default());
    assert_eq!(e.restore(vec![], R), Ok(0));
}

// Item 8: restoring emits nothing, and the dedup memory starts empty: a
// `permission_prompt` Notification within 10 s of a request from before the
// restart is a new episode (a false knock, never a lost one).
#[test]
fn item8_the_dedup_memory_is_not_restored() {
    let mut e = Engine::new(Settings::default());
    let t = T0 - 3000;
    e.step(Input::Event {
        event: request(t),
        simulated: false,
    });
    e.step(Input::Event {
        event: ev(Kind::Activity, t + 1000),
        simulated: false,
    });
    let snap: Vec<_> = (e.sessions().iter())
        .map(|s| Saved::of(s, Liveness::Alive))
        .collect();
    // Without a restart the prompt is a duplicate.
    let prompt = ev(Kind::NeedsPermission, t + 6000);
    let fx = e.step(Input::Event {
        event: prompt.clone(),
        simulated: false,
    });
    assert_eq!(fx, vec![]);
    let mut e = Engine::new(Settings::default());
    e.restore(snap, t + 5000).unwrap();
    let mut h = H(e);
    assert_eq!(
        h.send(prompt),
        vec![notify(Reason::NeedsPermission), Persist]
    );
}

// Round trip: what the engine holds survives save → restore, apart from the
// clocks, the dedup memory and who was waiting.
#[test]
fn a_snapshot_restores_to_the_same_sessions() {
    let mut e = Engine::new(Settings::default());
    let step = |e: &mut Engine, ev: Event| {
        e.step(Input::Event {
            event: ev,
            simulated: false,
        })
    };
    let mut a = request(T0);
    a.session_id = "a".into();
    step(&mut e, a);
    let mut b = ev(Kind::TurnCompleted, T0 + 1);
    b.session_id = "b".into();
    step(&mut e, b);
    let mut c = ev(Kind::Activity, T0 + 2);
    c.session_id = "c".into();
    c.detail.category = Some(Category::Web);
    step(&mut e, c);
    let snap: Vec<_> = (e.sessions().iter())
        .map(|s| Saved::of(s, Liveness::Alive))
        .collect();
    let mut r = Engine::new(Settings::default());
    assert_eq!(r.restore(snap.clone(), R), Ok(3));
    let back: Vec<_> = (r.sessions().iter())
        .map(|s| Saved::of(s, Liveness::Alive))
        .collect();
    assert_eq!(back, snap);
    assert!(r.sessions()[1].turn_done());
}
