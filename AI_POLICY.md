# AI policy

Most of this repository was written by AI agents, and their use is encouraged.
What a contribution has to withstand is the same however it was produced, and
this page says what that is.

## Getting oriented

An assistant is good at entering a codebase of this shape: ask it to trace an
instruction from `decoded` through the `arrayFold` step to a committed write-log
row, or to explain what the batch commit is protecting.

It will not give you the intuition, and this project punishes its absence. Two
documents state properties that are not visible in the code. `PURITY.md` says
what "runs in ClickHouse" means. `SPEC.md` says what the CPU, the memory map,
the MMIO surface and the trace format are. Read them yourself before changing
execution semantics.

## You own what you submit

Opening a pull request or an issue means vouching for it. You should be able to
say what the change does, why it is correct, and how it fits the invariants
above. If a reviewer asks a question, answer it in your own words rather than
relaying a model's.

That applies whether you wrote every line, used an assistant, or anything
between. The bar is whether somebody understands the change.

## Correctness claims need evidence

This is the section that matters most here.

Non-negotiable #5 in `CLAUDE.md` says a check that never ran is
indistinguishable from one that passed. The failure it names is machinery that
reports success while verifying nothing, and a pull request written by a model
is an efficient way to produce it, because a plausible explanation of why a
change is correct reads exactly like a correct change.

So a claim in any of these three classes is judged on evidence, not on
reasoning:

- **Purity.** Show `scripts/check_purity.sh` failing before your change and
  passing after, naming the rule it enforces.
- **Divergence.** Show a checkpoint trace from both sides, with the icount
  where they agree or disagree. `make diff N=<count>` produces it.
- **Throughput.** Show a before-and-after measurement, on the same machine, with
  the ClickHouse version and K recorded, and say how quiet the machine was.

Pasting a command's real output is required. "Tests pass" is not evidence.

## Quality over volume

One clear problem and one clear solution per pull request gets read and merged
faster than a large change doing several things.

- Tie it to a need, ideally an open issue.
- Run `make lint` and check it by exit code.
- A diff past roughly 400 lines is worth splitting, or at least flagging.
- Skip the drive-by cleanup. A formatting sweep bundled with a fix makes the fix
  harder to review and harder to revert.

## Agentic contributions

A pull request written by an agent is held to the bar above: understood by the
person submitting it, covered by tests, and backed by real output where the
change warrants it.

If an agent did most of the work, say so and name the model in the pull request
description. That is not required when a tool assisted, it is not held against
the change, and it gives whoever reviews it useful context.

Commits and pull request bodies carry no AI attribution trailers or footers: no
`Co-Authored-By` for a model, no "generated with" line. Commit messages do not
reference plans, sessions or iterations.

## Licensing

Contributions are accepted under the terms of the directory they land in.
`LICENSING.md` has the boundary. Inbound is the same as outbound and there is no
CLA. If you reproduce code from elsewhere, whether you found it or a model
produced it, you are responsible for its licence being compatible and for saying
where it came from.

---

If something is not covered here, the principle underneath it is: be considerate
of reviewers' time, and take ownership of what you submit.
