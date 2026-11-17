# 0037: Restoring sessions after a restart

- **Status:** Approved by user on 2026-11-17
- **Date:** 2026-11-17
- **Spec sections:** §5 (FR-13), §8.4, §9.7 (Process liveness), §11 (rule 4)
- **VERIFY item:** none

## Context

FR-13 says Knock restores all session states within 2 s after an app restart: "Snapshot restored; process liveness re-checked". Knock already saves `sessions.json` on every change, but it never reads it back.

While Knock is closed it hears nothing from Claude Code. So a restored state can be out of date: a session that was asking for permission may have been answered, a working session may have finished. §8.4 has no rows for this. The snapshot also holds no agent ids (ADR 0019), so Knock can't tell which agent in a session was waiting.

## Decision

The user chose items 2 and 3 on 2026-11-17 and approved the other items, proposed by the lead, the same day.

1. **Sessions whose program is still running** come back in the state, with the animal and the "time in state", that they had when Knock closed. Simulated sessions are never restored.
2. **A session that was in Needs you** comes back as it was: knocking if the user had not clicked it, calm if they had (ADR 0016). It is the same episode, so Knock sends no second notification for it. (User's choice. Rejected: every restored Needs-you session comes back calm.)
3. **A session whose program is gone** when Knock starts is treated by the existing §8.4 row "Process gone without `session.ended`": it shows "Session ended" and fades after 60 s. A Failed session stays Failed until dismissed, as today. (User's choice. Rejected: dropping such sessions without showing them.)
4. **Clocks restart at start-up.** Knock sends no notification about time it did not watch:
   - An unanswered, unseen episode that has not escalated yet escalates `escalationMinutes` after Knock started, if it is still open. An episode that already escalated never escalates again.
   - A Working session becomes Stalled only after `stallMinutes` with no events since Knock started.
   - The 6-hour removal rule keeps using the saved time of the last event.
5. **Unknown requesters.** For a restored episode, the first `activity` event from that session, from any agent, closes it as if the last waiting requester had resumed. New episodes follow ADR 0019 as before. Pinned after the fresh review of 2026-11-17, because the first implementation got this wrong and lost a knock:
   - Only `activity` may stand in for the unknown requester. `turn.completed` removes the main agent alone and never the unknown one, so a restored episode whose sub-agent was waiting keeps knocking when the main agent's turn ends, exactly as the §8.4 `turn.completed` row already requires. The alternative silently turned such a session into "Done, ready to review" while a permission prompt sat unanswered, which breaks §6 Reliability.
   - Accepted cost: a restored session that finishes its turn having emitted no `activity` at all keeps knocking until the next `activity`, `turn.started`, `agent.failed` or `session.ended`, or until the user clicks it, which marks it seen so it goes calm and never escalates. A lingering knock the user can clear beats a silently lost one.
   - An `activity` from an agent that was never a requester also stands in for the unknown one, so a restored episode can close one event earlier than the rule above describes. Accepted: it never loses a requester.
6. **A session with no recorded process** is restored as it was and follows the existing 6-hour rule (ADR 0018).
7. **A missing, unreadable or invalid `sessions.json`**, or one with an unknown `version`, means Knock starts with no sessions. Knock logs the kind of problem only, and the next save replaces the file. This includes an entry holding an unknown state, needs kind, activity or precision: the whole file is dropped rather than partly guessed at.
8. Restoring emits no `Notify`, `Escalate` or `ReadyBatchAdd`. The 10-second deduplication memory is not restored.

## Consequences

- SPEC.md §8.4 gets a "Restore" paragraph that points here. No §8.5 change: the strip already loads sessions with `get_sessions()`.
- `sessions.json` may gain fields the restore needs (for example whether the main agent's turn was done, ADR 0021). They stay inside §11 rule 4: no agent ids, no cwd, no free text.
- `core-windows` adds a pure restore entry point to `knock-core`, with engine tests and the §8.4 invariants run over restored snapshots, and the load-and-recheck step in the app shell.
- This touches §11, so the change gets a fresh review (ADR 0029).
- Known limit: a restored Needs-you session that was answered while Knock was closed keeps knocking until its next event or a click.
- The health check's "last event from Claude Code" reads the newest saved last-event time after a restore, so it reports a real time instead of "hasn't heard since it started" (FR-12). This also feeds `GET /v1/health` and `knock doctor` (lead, 2026-11-17).

## Evidence

none
