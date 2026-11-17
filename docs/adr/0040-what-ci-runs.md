# 0040: What CI runs, and where the visual tests run

- **Status:** Approved by user on 2026-11-17
- **Spec sections:** §13.4 (CI), §12.1 (UI visuals row), §12.4
- **Date:** 2026-11-17
- **VERIFY item:** none

## Context

§13.4 says `ci.yml` runs `pnpm check`, `pnpm test:visual`, `pnpm art:check`, `cargo-deny`, `pnpm audit`, a Windows build and a smoke test. The workflow actually runs only `pnpm check`, `cargo fetch --locked`, `cargo-deny` and `pnpm audit`. Four of the listed checks have never run on GitHub, so the lead running them by hand is the only thing that catches a failure.

The visual tests are the reason this matters now. They compare screenshots pixel by pixel against `maxDiffPixels: 10` and use the installed Edge, deliberately, because it is WebView2's engine (§12.1). On 2026-11-17, five overlay screenshots failed by 13 to 122 pixels although nothing in `apps/desktop/ui/overlay/` or `art/` had changed since the baselines were made on 2026-11-04: the user's Edge had updated, and anti-aliasing on curves and glyph edges moved. The lead confirmed from the diff images that no text, layout, element or pose had changed.

A GitHub runner has its own Edge, which updates on its own schedule. Running the visual tests there would make every pull request depend on the runner's Edge matching the committed baselines.

## Decision

The user chose on 2026-11-17:

- `ci.yml` gains the three checks that give the same answer on every machine: `pnpm art:check`, the Windows build, and the smoke test (§12.1 "Smoke" row: install, launch, `knock sim`, read the debug snapshot).
- `pnpm test:visual` stays a local check. The lead MUST run it and report its result before merging any branch that touches UI or art, as part of the §12.4 review.
- The threshold stays `maxDiffPixels: 10`. A changed word must still fail.
- When the visual tests fail only because the browser drifted, the lead has an agent confirm from the diff images that each difference is anti-aliasing on edges alone, with no changed text, layout, element or pose, before any baseline is regenerated. A real difference is a bug to fix, never a baseline to bless.

## Consequences

- SPEC.md §13.4 is amended: the `ci.yml` row lists `pnpm check`, `pnpm art:check`, `cargo-deny`, `pnpm audit`, the Windows build and the smoke test, and points here for the visual tests.
- `core-windows` changes `.github/workflows/ci.yml`. The build and smoke steps make CI slower; that is accepted.
- Knock keeps a gap: a UI regression that only a screenshot would catch is caught by the lead, not by GitHub. The user accepted this to keep pull requests from failing for reasons unrelated to the change.
- `nightly.yml` is unchanged by this ADR.

## Evidence

`pnpm test:visual` on the user's machine, 2026-11-17: 220 passed, 5 failed, all in `apps/desktop/ui/overlay/visual/screenshots/`, by 13 to 122 pixels. The lead opened the `icons` diff image: isolated pixels on curves and glyph edges only. Last overlay baseline commit: 1ff0a25, 2026-11-04. No overlay or art file changed between those dates.
