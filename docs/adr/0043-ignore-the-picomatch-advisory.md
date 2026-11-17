# 0043: Ignore the picomatch advisory until a fix exists

- **Status:** Approved by user on 2026-11-18
- **Date:** 2026-11-18
- **Spec sections:** §11 (rule 10), §13.2, §13.4
- **VERIFY item:** none

## Context

On 2026-11-18 the first pull request to run the amended `ci.yml` failed at `pnpm audit`:

```
high  picomatch vulnerable to ReDoS through single-branch repeated extglobs
      Vulnerable versions <=4.0.7   Patched versions >=4.0.8
      GHSA-fc5q-39xc-vjpf
```

The lead established:

- **No fix exists.** `npm view picomatch versions` lists up to `4.0.7`, and `latest` is `4.0.7`. The advisory names `4.0.8` as patched, but that version has never been published, so there is nothing to upgrade to and no override to pin.
- **It does not ship.** picomatch reaches the tree only as a transitive dev dependency of `vite` (`vite > picomatch`, `vite > tinyglobby > fdir > picomatch`, 12 paths in all). The built front-end in `apps/desktop/ui/dist` contains no picomatch, and the NSIS installer carries the Rust binary and the built assets, so no user's machine ever runs it.
- **The input is ours.** The flaw is a regular-expression denial of service reachable through a crafted glob pattern. The only patterns picomatch sees here come from this repository's own Vite, Vitest and Playwright configuration. Feeding it a hostile pattern would require already controlling the repository.

§11 rule 10 requires `pnpm audit` to run in CI, so suppressing a finding is a decision about a MUST rule and not the lead's to take alone.

## Decision

The user chose on 2026-11-18: ignore this one advisory, by id, until a fixed picomatch is published.

- The root `package.json` gains `pnpm.auditConfig.ignoreGhsas: ["GHSA-fc5q-39xc-vjpf"]`, with a comment pointing here.
- Only this advisory is ignored. `pnpm audit` keeps running in CI on every pull request and still fails on anything else, including a new advisory against picomatch.
- The entry is removed as soon as a picomatch release fixes it. Whoever next touches the JavaScript dependencies checks whether it can go, and the lead checks before each release.

## Consequences

- SPEC.md needs no change: §11 rule 10 still holds, because `pnpm audit` still runs and still gates the pull request.
- Knock carries a known, unfixable advisory in its build tooling for as long as this entry stands. Accepted because it cannot reach a user and cannot be fixed by upgrading.
- If picomatch publishes a fix and the entry is forgotten, CI will not remind anyone: an ignored id stays ignored. That is the cost of this decision, and the reason it is written down here rather than only in `package.json`.

## Evidence

`pnpm audit` on `develop` at 2026-11-18: one high finding, GHSA-fc5q-39xc-vjpf, 12 paths, all under `apps__desktop__ui>vite`. `npm view picomatch version` → `4.0.7`; `npm view picomatch@4.0.8 version` → 404. `grep -rl picomatch apps/desktop/ui/dist` → no match. `pnpm` 11.5.3.
