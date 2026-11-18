# 0039: Setup opens by itself only until it has been finished once

- **Status:** Approved by user on 2026-11-17
- **Date:** 2026-11-17
- **Spec sections:** §10.4, §8.6 (`launchAtSignIn`), §8.1 (Repair)
- **VERIFY item:** none

## Context

Today the setup window opens by itself at start on the first run, and at every start while Knock isn't connected to Claude Code. So after the user chooses **Remove Knock from Claude Code** while Knock starts at sign-in, setup opens at every sign-in. The pre-release review reported this (finding 4, 2026-11-06) and it was deferred to Phase 5. §10.4 doesn't say when setup opens by itself.

On 2026-11-17 the user chose: setup opens by itself only the first time; after that Knock starts quietly.

## Decision

- Setup opens by itself at start only while the user has never answered the "Start Knock when you sign in?" step, that is, while `launchAtSignIn` is `null` (§8.6). This covers the first run. It also covers a setup that was closed before the user got that far (lead's addition, approved by the user, so a setup closed by accident is offered again).
- After that, Knock never opens setup by itself. If it isn't connected to Claude Code, Settings and the health check say so, and setup can be reopened from Settings (§10.4).
- Unchanged: if the server can't start, the Repair screen still opens at start (ADR 0023).

## Consequences

- SPEC.md §10.4 gets one sentence that points here. No contract changes and no new setting.
- `core-windows` changes the start-up rule in the app shell, with a test.

## Evidence

none
