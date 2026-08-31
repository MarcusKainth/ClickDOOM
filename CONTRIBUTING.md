# Contributing to ClickDOOM

## What is worth contributing

The most useful contribution this project can receive is one that falsifies its
central claim: a case where the SQL CPU and the reference emulator disagree on
the same ROM, or a place where computation has left SQL. Both go ahead of any
feature.

After those come the checks themselves. A check that never ran is
indistinguishable from one that passed, and this repository has shipped
machinery that reported success while verifying nothing more than once.
Finding another instance is worth more than most features.

Then throughput, correctness fixes with a reproduction attached, and
documentation that makes any of the above easier to attempt.

## Before you start

Open an issue first for anything large, anything that changes `SPEC.md` or
`PURITY.md`, and anything you would be unhappy to see closed unmerged.

Blank issues are off. Each form asks the questions somebody would have to ask
you anyway, and the divergence form in particular collects the fields that make
a report reproducible instead of interesting.

Labels say what is open:

- `good first issue` and `help wanted` need no permission. Start, and say so on
  the issue so nobody duplicates you.
- `needs: decision` and `needs: maintainer` are waiting on a call that has not
  been made. A pull request against one is likely to be wasted work.
- `area:` labels are topics, not owners. Nobody owns a directory here.

A maintainer should respond within a few days, though it can take longer.

## Building and testing

You need Docker, `uv`, `cargo`, `make`, `shellcheck` and `clang-format`.
`DEVELOPING.md` covers versions and the rest of the mechanics.

    make help    # every target, grouped
    make gates   # every check CI runs on a pull request
    make test    # every suite that has one

`make gates` calls the same targets CI does, so a target that passes here is
what runs there. Expect upwards of ten minutes, most of it the ROM build and the
differential smoke.

It is necessary and not sufficient. The nightly deep-diff is the only run that
compares memory, and no pull request makes it.

Check by exit code. A pipeline reports only its last command's status, so
`make gates | tail` can hide a failure.

## Opening a pull request

Fork, branch as `scope/short-desc`, and open the pull request against `main`.
Nobody pushes to `main` directly, maintainers included, and the branch rules
enforce it.

A workflow from a fork needs maintainer approval before it runs. A workflow runs
as it exists in the pull request, so an unreviewed run is an unreviewed change
to what CI proves. It costs you one round-trip.

Title is `scope: imperative summary`, at most 72 characters.
The scopes are `spec`, `rom`, `refemu`, `sqlcpu`, `executor`, `driver`,
`render`, `test`, `bench`, `ci` and `docs`. A change that breaks a contract is
`scope!:`.

**Evidence is required, and it is the part most pull requests get wrong.** Paste
the actual command and its actual output. "Tests pass" is not evidence. A
purity claim shows the check failing before and passing after, naming the
`PUR-N`. A divergence claim shows a checkpoint trace from both sides with the
icount. A throughput claim shows a before-and-after on the same machine, with
the ClickHouse version and K recorded, and says how quiet the machine was.

The template asks which invariants the change touches. Answer it by reading
`PURITY.md`, not from memory.

## The invariants

`PURITY.md` numbers its rules `PUR-N` and is the only place that states
them. `SPEC.md` is the contract everything else is checked against.
Cite them by number rather than restating them: "this touches PUR-10" is the
reviewable form.

Most purity rules cannot be grepped and are enforced by review alone.
`PURITY.md`'s Enforcement table says which.

## Working with AI assistance

Encouraged, and used heavily on this project. `AI_POLICY.md` states the bar:
you own what you submit, you answer review questions in your own words, and a
correctness claim needs evidence rather than a plausible explanation. If an
agent did most of the work, name the model in the description. That is not held
against the change.

Git carries no AI attribution: no `Co-Authored-By` for a model, no "Generated
with" footer.

## Security and legal

Report anything exploitable privately through
[GitHub's advisory flow](https://github.com/MarcusKainth/ClickDOOM/security/advisories/new)
rather than as a public issue. `SECURITY.md` says what is in scope, and the
emulator boundary is narrower than people expect.

Contributions are accepted under the terms of the directory they land in:
Apache-2.0 outside `rom/`, GPL-2.0-or-later inside it. Inbound is the same as
outbound. There is no CLA and nothing to sign. `LICENSING.md` has the boundary.

`CODE_OF_CONDUCT.md` applies. Arguing forcefully that one of this project's
claims is wrong is welcome, however bluntly put. Making it about the person is
not.
