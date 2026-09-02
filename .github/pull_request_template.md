<!--
Delete every section that does not apply, and every comment like this one. If
the commit message already carries the argument, a link and a sentence is a
complete description.

Title is `scope: imperative summary`, at most 72 characters.

Contributions are accepted under the terms of the directory they land in:
GPL-2.0-or-later inside rom/, GPL-3.0-or-later inside native/, Apache-2.0 everywhere else. There is no CLA to sign.
-->

## What this changes, and why

<!--
`Closes #123.` on a line of its own first, if an issue tracks this.

No need to repeat what that issue says. The diff shows the code and the issue
states the problem, so use this space for what neither can tell a reviewer: the
approach, the decisions that were not forced, and what someone running this
sees differently.

When no issue tracks this, state the problem here, in terms of what someone
running it hits. "`commit.py` writes the wrong high-water mark" is a symptom of
the implementation. "A resumed run replays stores it already committed" is the
problem.

Say where this delivers less, more, or something other than the issue asked
for. That comparison is the one a reviewer cannot make alone.
-->

## Evidence

<!--
Paste the actual commands and their actual output. "Tests pass" is not
evidence, and a green CI badge is not evidence that the thing you changed is
covered.

  Purity      the check failing before and passing after, naming the PUR-N.
  Divergence  a checkpoint trace from both sides, with the icount.
  Throughput  before and after on the same machine, with the ClickHouse
              version and K, and how quiet the machine was.
  rom/        `make build-rom && make -C rom check-pinned-hash`.

If a check you are relying on has never actually failed, say so. A check that
never ran is indistinguishable from one that passed.
-->

```
$ make ...
```

## Invariants

<!--
Name the PUR-N rules this change touches and say how each property still holds.
Touching one is not a problem. Answer "None." if it touches none.

PURITY.md states each rule in full and is the only place that does. Most are
enforced by review alone, and its Enforcement table says which.
-->

## Spec impact

<!-- Delete all but one. -->

- [ ] None. No contract in SPEC.md is touched
- [ ] SPEC.md changes are included, and a `spec-change` issue tracks the decision

## Checks

<!-- Tick what you ran, by exit code. Nothing else belongs under this heading. -->

- [ ] `make gates`. If you could not run it, say which suites you ran instead
- [ ] No AI attribution trailers in the commits

## Anything else

<!--
Most changes delete this section.

Keep it to point a reviewer at the lines worth their attention, to say how to
reject one part without blocking the rest, or to report something you verified
by hand that no automated test captures. A change with no runtime behaviour has
nothing of that last kind to report.

A defect you found along the way and left alone belongs in its own issue rather
than here.
-->
