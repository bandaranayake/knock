# 0041: Knock has no nightly workflow

- **Status:** Approved by user on 2026-11-18
- **Date:** 2026-11-18
- **Spec sections:** §13.4 (CI), §12.1 (Smoke), §12.2
- **VERIFY item:** none

## Context

§13.4 has always listed a `nightly.yml`: "Push to main — same, plus an installer artifact". While brief 5.6 added the missing checks to `ci.yml` (ADR 0040), `core-windows` reported that `.github/workflows/` holds only `ci.yml` and `release.yml`. The nightly workflow has never existed, in any phase.

Nothing depends on it. Pull requests are checked by `ci.yml`, which now builds the installer and runs a smoke test. Releases are built by the manual `release.yml` (ADR 0036) and published by hand. Knock has one developer, so pushes to `main` are rare and always follow a reviewed pull request that CI has already checked.

## Decision

The user chose on 2026-11-18: Knock has no nightly workflow, and §13.4 drops the row. If a build of `main` is ever wanted without making a release, the Release workflow already produces one as a draft.

Also decided while adding the smoke test:

- CI builds the installer and checks exactly one `*-setup.exe` exists, but does not install it. Nobody has verified how Tauri's NSIS installer behaves unattended, and `core-windows` declined to write a step it could not trust. Install, update and uninstall stay in the §12.2 release matrix the user runs by hand (user, 2026-11-18).
- No Rust build cache in CI. It would need a new third-party action, and caching `target/` would break the "exactly one installer" check, because installers from earlier versions accumulate there. ADR 0040 already accepts a slower CI.

## Consequences

- SPEC.md §13.4 loses the `nightly.yml` row, and §12.1's "Smoke" row gains a pointer here, since "Install" is covered by the user's manual test rather than by CI.
- No workflow file is added or removed: `nightly.yml` never existed.
- Knock keeps no automatic build of `main`. Accepted: `ci.yml` has already checked the same commit on its pull request.

## Evidence

`.github/workflows/` on `develop` at 2026-11-18 holds `ci.yml` and `release.yml` only. `git log` shows no `nightly.yml` in any phase.
