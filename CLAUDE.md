# CLAUDE.md — ClickDOOM agent handbook

You are working on ClickDOOM: the actual 1993 DOOM engine, compiled to
bare-metal RV32IM, executed by a CPU implemented in ClickHouse SQL. Not a
raycaster written in SQL — a CPU emulator in SQL running the real binary.

**Read `SPEC.md` and `PURITY.md` before writing any code.** SPEC.md is the
contract; PURITY.md is the definition of done for the whole project. If your
change needs the contract to move, file a `spec-change` issue — do not
"temporarily" diverge.

## Workstreams and ownership

| Scope      | Path        | Charter                                                            |
|------------|-------------|--------------------------------------------------------------------|
| `rom`      | `rom/`      | doomgeneric → bare-metal RV32IM: crt0, linker script, libc shims, MMIO glue, embedded WAD, reproducible build (SPEC §4) |
| `refemu`   | `refemu/`   | Reference RV32IM interpreter (Python) — the oracle. riscv-tests green; emits SPEC §7 traces |
| `sqlcpu`   | `sqlcpu/`   | Instruction decode/execute in ClickHouse SQL; `schema.sql`; riscv-tests harness running inside ClickHouse |
| `executor` | `executor/` | arrayFold batch loop, write-log memory, batch commit, MMIO plumbing (SPEC §6) |
| `driver`   | `driver/`   | Thin Python loop + ANSI frame blitter. Bound by PURITY.md — no logic |
| `render`   | `driver/`   | The frame-readout SQL (lives beside the driver, but the query is SQL-side computation) |

One scope per PR. Cross-scope changes need team-lead sign-off in the PR.

## Coordination protocol

- The backlog is GitHub Issues. **Self-assignment is the claim** — assign
  yourself before starting; never work an issue assigned to someone else.
  Unassigned = available.
- Found a problem mid-task? File it with the right issue form (divergence
  reports use `divergence-report`) and keep going unless it blocks you.
  Label `blocked` + reference the blocking issue if it does.
- Disagreements: author and reviewer try once in PR comments → team lead
  decides → still stuck, label `needs-human` and move to other work.

## Git conventions

- Commit/PR title: `scope: imperative summary` ≤72 chars. Scopes: `spec`,
  `rom`, `refemu`, `sqlcpu`, `executor`, `driver`, `render`, `test`,
  `bench`, `ci`, `docs`. Breaking contract change: `scope!: ...`.
- Branch names: `scope/short-desc`.
- Squash-merge only; the PR title becomes the commit. CI lints it.
- **Never `--delete-branch` while another PR is based on that branch.**
  GitHub *closes* a PR whose base branch disappears — it does not retarget
  it. This has cost us three PRs (#36, #38, #64); each needed the base
  branch recreated before GitHub would even reopen them, and one could not
  be recovered at all. Merge, then retarget the dependent PR to `main`,
  then delete. **Stack depth is irrelevant here** — one dependent PR is
  enough, so a two-deep stack fails exactly the same way.
- **Keep stacks shallow — one PR in flight, two at most.** Different
  problem, different evidence: squash-merge collapses a branch into a
  commit sharing no history with it, so every merge forces a rebase and a
  full CI cycle on every PR stacked above. A four-deep stack cost four
  sequential rebase-and-recheck rounds. This is a throughput tax, not a
  correctness hazard — the rule above is what protects the PRs.
- **Force-pushing does not reliably retrigger CI.** GitHub's
  `pull_request` synchronize event is unreliable after a force-push, so a
  rebased PR can sit with zero check-runs and look merge-blocked for no
  reason. Push an empty commit to retrigger.
  **Note (2026-08-26):** CI evidence gathered this day after 15:11 UTC
  is unreliable — GitHub Actions was in a confirmed platform-wide major
  outage (incident opened 15:11 UTC "degraded availability for
  Actions," Pages also degraded 15:12 UTC, database-primary failover
  15:23 UTC — checked directly against githubstatus.com, not relayed).
  Runs from that window show `startup_failure` and jobs wedged
  indefinitely in `queued`, indistinguishable from "the event never
  fired" via `gh run list`. Several retrigger remedies (this one, a
  real content-touch commit, closing and reopening the PR) were tested
  that afternoon; those results are contaminated and were **not** used
  to change this entry — the advice above is unconfirmed, not
  disproven. If you are re-testing retrigger behaviour, check
  githubstatus.com first.
- Merging: author merges after (a) CI green and (b) one approval from a
  different agent — preferably your contract counterpart (`rom`↔`refemu`,
  `sqlcpu`↔`executor`). Never approve your own PR. SPEC/PURITY/workflow
  changes additionally require the human owner (CODEOWNERS enforces this).
- Reviewers: re-run the evidence commands yourself; don't trust pasted
  output. Check the purity items every time.

## Canonical commands

Use `just` recipes only — do not improvise shell incantations; if a recipe
is missing, add it in the same PR.

    just up            # pinned ClickHouse via docker compose
    just build-rom     # reproducible ROM build (dockerized toolchain)
    just test-refemu   # riscv-tests against the reference emulator
    just test-sqlcpu   # riscv-tests inside ClickHouse
    just diff N        # differential run, N instructions, report first divergence
    just smoke         # CI-sized differential smoke (1M instructions)
    just bench         # executor throughput benchmark (instructions/sec)
    just lint          # all linters + purity check

## Phase plan

- **Phase 0 (single session, Opus):** arrayFold throughput benchmark;
  resolve SPEC §9 open questions; ratify SPEC 0.1.0. Output: ADR-0001
  accepted or replaced, SPEC un-drafted.
- **Phase 1 (team):** parallel build of `rom`, `refemu`, `sqlcpu`,
  `executor`+`driver`. Milestone: riscv-tests fully green inside
  ClickHouse; ROM boots in refemu.
- **Phase 2 (small):** integration + executor performance. Milestone:
  DOOM reaches its first `FRAME_COMMIT` in ClickHouse.
- **Phase 3 (team, divergence hunt):** `-timedemo demo3` runs to
  completion, no desync, final-frame hash matches refemu. Then: timelapse,
  README victory lap.

## Non-negotiables

1. Determinism (SPEC §8). `now()`, `rand()` and friends never touch a
   computation path.
2. Purity (PURITY.md). When in doubt whether something is "computation",
   it is.
3. Don't edit `rom/PINNED_HASH`, SPEC, or PURITY to make a red check green.
   Red checks are information.
4. Divergences get filed with full repro fields even if you fix them in the
   same PR — the report history is the project's debugging memory.
5. **A check that never ran is indistinguishable from one that passed —
   require positive evidence, never infer success from the absence of
   failure.** One shape, everywhere it shows up: `SELF_MODIFY` correct
   but unreachable through both driver call sites, `HALT_EXIT` with zero
   coverage anywhere, `tgt_mis=1` unreachable in a 53-row fixture, a gate
   4 smoke test that never actually self-modifies, `gh pr checks`
   reporting "no checks reported" reading identically to all-green if
   your own check counts non-`pass` lines instead of requiring passes for
   the commit you actually care about (see the CI entry above — that
   near-miss is what surfaced this rule). Before trusting a check —
   automated, in review, or in a PR's own evidence section — confirm it
   actually ran against what you think it ran against.
