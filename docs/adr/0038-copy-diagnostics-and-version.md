# 0038: Copy diagnostics and the version in Settings

- **Status:** Approved by user on 2026-11-17
- **Date:** 2026-11-17
- **Spec sections:** §8.5 (commands), §11 (rules 3, 4, 5, 8), §7.1
- **VERIFY item:** none

## Context

§11 rule 8 says diagnostics are shared only through a **Copy diagnostics** button, but no phase builds the button and §8.5 has no command for it. Beta testers also can't see which version they run: they read it from the installer's file name. On 2026-11-17 the user chose to add both in Phase 5.

## Decision

Add one command to §8.5:

| Command             | Purpose                                                                      |
| ------------------- | ---------------------------------------------------------------------------- |
| `get_diagnostics()` | Returns `{ version: string, text: string }` for Settings and Copy diagnostics |

- `version` is Knock's version, for example `0.3.0-beta.1`. Settings shows it.
- `text` is plain text made by the core. The **Copy diagnostics** button in Settings puts it on the clipboard. Nothing is sent anywhere.
- `text` holds only:
  - Knock's version, and the Windows version and build.
  - The health check results (`run_health_check()`): each label, pass or fail, and its plain-language detail.
  - The number of sessions per tier and per precision level.
  - These settings: `mode`, `size`, whether `monitor` is `"primary"`, `stallMinutes`, `escalationMinutes`, `notifyOnStall`, `pauseNotifications`, whether quiet hours are on, `launchAtSignIn`.
  - The number of displays, and each one's scaling.
- `text` never holds: agent content of any kind, the token, the port, a session id, a project name, a cwd hash, a user name, or any path beyond a file name.

## Consequences

- SPEC.md §8.5 gets the new row.
- `core-windows` builds the command, with a test that the text holds none of the excluded items. `ui` adds the version line and the button to Settings. No new dependency: the button uses the browser clipboard.
- The README gets a short privacy note that says exactly what Copy diagnostics copies and what it never copies (user, 2026-11-17). The lead adds it when the button is merged.
- This touches §11, so the change gets a fresh review (ADR 0029).
- Project names are left out although §11 rule 4 allows storing them, because testers paste this text into public GitHub issues.

## Evidence

none
