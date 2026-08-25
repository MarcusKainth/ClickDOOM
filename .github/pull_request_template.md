<!-- Title must be `scope: imperative summary` (≤72 chars); `scope!:` if it breaks a contract. CI lints this. -->

## Summary

<!-- What and why, 2–5 sentences. Link the issue: Fixes #N -->

## Scope

<!-- Exactly one: spec | rom | refemu | sqlcpu | executor | driver | render | test | bench | ci | docs.
     Cross-scope changes require team-lead sign-off — name the approving lead session here. -->

## Spec impact

- [ ] None — no contract in SPEC.md is touched
- [ ] SPEC.md change included (requires human-owner approval; `spec-change` issue: #N)

## Test evidence

<!-- Paste the actual commands and their real output. "Tests pass" is not evidence.
     rom/refemu/sqlcpu: riscv-tests pass count. executor: `just bench` before/after.
     Anything touching execution semantics: `just diff` result. -->

```
$ just ...
```

## Purity declaration

- [ ] No computation moved outside SQL (no executable UDFs / subprocess delegation)
- [ ] Driver changes (if any) remain within PURITY.md's four allowed actions
- [ ] No wall-clock/randomness on any computation path

## Author checklist

- [ ] CI green
- [ ] Tests added/updated for new behavior
- [ ] No unrelated diffs
- [ ] Title matches the commit convention

---

## Reviewer checklist (different agent than author; contract counterpart preferred)

- [ ] Re-ran the evidence commands locally — outputs match the paste
- [ ] Change complies with SPEC.md as written (not as the author wishes it were)
- [ ] Determinism: explicit ORDER BY where result-affecting; no block-order reliance
- [ ] Purity items verified by reading the diff, not the declaration
- [ ] For `executor`/`sqlcpu`: no perf regression >10% without an ADR justifying it
