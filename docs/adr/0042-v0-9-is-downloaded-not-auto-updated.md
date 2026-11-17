# 0042: v0.9 is downloaded, not auto-updated

- **Status:** Approved by user on 2026-11-18
- **Date:** 2026-11-18
- **Spec sections:** §13.5 (Versions), §13.4, §14 (Phase 6), §11 rule 9
- **VERIFY item:** none

## Context

§13.5 has always said v0.9 reaches "Same testers, via auto-update". Knock has no auto-updater: there is no Tauri updater plugin in `apps/desktop/src-tauri/Cargo.toml` or `tauri.conf.json`, nothing generates `latest.json`, and no updater key exists. §14 puts the updater, `release.yml`'s tag trigger and `latest.json` in Phase 6, and ADR 0036's own comments say the same. So §13.5 describes a delivery route that cannot exist until after v0.9 ships.

## Decision

The user chose on 2026-11-18: v0.9 is published as a GitHub pre-release and testers install it over their existing copy by hand, exactly as they did for v0.3.0-beta.1. The updater stays in Phase 6, where §11 rule 9's key handling is planned.

- §13.5's v0.9 row becomes "Same testers, downloaded from a GitHub pre-release".
- The release is built by the manual `release.yml` (ADR 0036) and published by hand, as every release is (§13.4).
- Testers MUST still be told the installer is unsigned and asked about Smart App Control (§13.5), as for v0.3.
- The README already notes that a manual upgrade keeps settings, and that a manual upgrade with "Run Knock" unticked leaves sign-in off until Knock is started once.

## Consequences

- SPEC.md §13.5's v0.9 row changes. No other row changes: v1.0 is still "Public", and Phase 6 still delivers the updater.
- The §12.2 release matrix's "update" row is tested as a manual upgrade over an installed v0.3, not as an auto-update. Auto-update becomes a Phase 6 test.
- Knock ships v1.0 without ever having exercised auto-update on a tester's machine. Phase 6's Gate G3 ("clean install and update on a fresh Windows 11 machine") is where that gets covered.

## Evidence

`develop` at 2026-11-18: no `updater` in `apps/desktop/src-tauri/Cargo.toml` or `tauri.conf.json`; `latest.json` appears only in a `release.yml` comment that defers it to Phase 6; `apps/desktop/src-tauri/Cargo.toml` version is `0.3.0-beta.1`.
