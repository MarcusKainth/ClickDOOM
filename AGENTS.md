# ClickDOOM

The 1993 id Software DOOM engine, compiled to bare-metal RV32IM, executing on a
CPU implemented in ClickHouse SQL. Not a raycaster written in SQL. A CPU
emulator in SQL running the real binary.

[`CONTRIBUTING.md`](CONTRIBUTING.md) is the contributor-facing entry point.
[`DEVELOPING.md`](DEVELOPING.md) carries the build, test and benchmark mechanics
in full: read it for a target, a database or a bench convention.
[`AI_POLICY.md`](AI_POLICY.md) sets the bar a change has to clear.

`make help` lists every target. The tree tells you the rest, so this file does
not repeat it.

## Invariants

These documents state properties the code cannot show you, and both are cited
by name rather than restated.

[`PURITY.md`](PURITY.md) defines what "runs in ClickHouse" means, as numbered
`PUR-N` rules. Its Enforcement table says which of them
`scripts/check_purity.sh` reaches and which are review-only. Most have no
textual signature, so review is the only thing standing behind them.

[`SPEC.md`](SPEC.md) is the contract: the CPU, the memory map, the MMIO surface,
the batch execution contract, the trace format, the determinism rules. If a
change needs the contract to move, open a `spec-change` issue. Do not diverge
from it temporarily.

Touching an invariant is not automatically wrong. The change then has to say how
the property still holds, and citing the number is the reviewable form of that.

## Comments and commit messages

A comment says what the code does and what a caller may rely on. Why it is that
way and not the alternative belongs in the commit message, where it is dated and
attached to the diff. Apply that sentence by sentence: if deleting a sentence
changes nothing a reader would do differently, it goes.

- **Document what exists.** Not what was, not what will be. No "used to", no
  "not wired up yet", no "lands in a follow-up". A plan that needs recording is
  an issue.
- **State a fact once.** Copies drift. Cite the file that owns it.
- **No issue numbers, pull request numbers or SPEC section references in a
  comment.** A comment stands on its own. If it needs a reference to make
  sense, it is carrying an argument that belongs in the commit message.
- **Guardrails stay.** A check that looks redundant is usually the one that
  fired once.

The same rules hold in commit messages. The em-dash used as a dramatic pause,
the antithesis frame ("a bound on patience, not a deadline"), the evaluative
tail (", which is the whole point") and intensifiers that add nothing all read
as machine-written. Replacing one with another is not a fix.

## Commits and pull requests

Title is `scope: imperative summary`, at most 72 characters.
`CONTRIBUTING.md` lists the scopes. Branches are `scope/short-desc`.

Messages have to make sense to outsiders: no plan or phase references, no
shorthand that only resolves in one session. **No AI attribution in git.** No
`Co-Authored-By` for a model, no "Generated with" footer.

One logical change per commit. A diff past roughly 400 lines is worth
splitting. Skip the drive-by cleanup: a formatting sweep bundled with a fix
makes the fix harder to review and harder to revert.

## Non-negotiables

1. **Determinism.** `now()`, `rand()` and their relatives never touch a
   computation path.
2. **Purity.** When in doubt whether something counts as computation, it does.
3. **Never edit `rom/PINNED_HASH`, `SPEC.md` or `PURITY.md` to turn a red check
   green.** A red check is information.
4. **File divergences with full repro fields**, even when you fix them in the
   same pull request. The report history is the project's debugging memory.
5. **A check that never ran is indistinguishable from one that passed.**
   Require positive evidence; never infer success from the absence of failure.
   The shape recurs: a correct rule that nothing reaches, a case covered on one
   side of a differential only, a smoke test that stops short of the boundary it
   claims to cover, a checker whose own output nobody reads. Before trusting a
   check, automated or in review, confirm it ran against what you think it ran
   against.

## Done means

- `make lint` green, checked by exit code.
- Normative documents changed in the same commit as the behaviour they
  describe.
- Evidence in the pull request body: real command output, not a claim that
  tests pass.
- No unrun or failing tests handed over. If something is blocked, say which part
  and why, rather than narrowing the task to what passed.
