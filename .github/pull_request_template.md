<!-- Title must be `scope: imperative summary` (≤72 chars); `scope!:` if it breaks a contract. CI lints this. -->

## Summary

<!-- What and why, 2–5 sentences. Link the issue: Fixes #N -->

## Scope

<!-- Exactly one: spec | rom | refemu | sqlcpu | executor | driver | render | test | bench | ci | docs.
     Cross-scope changes require team-lead sign-off — name the approving lead session here. -->

## Spec impact

- [ ] None — no contract in SPEC.md is touched
- [ ] SPEC.md change included (requires maintainer approval; `spec-change` issue: #N)

## Test evidence

<!-- Paste the actual commands and their real output. "Tests pass" is not evidence.
     rom/refemu/sqlcpu: riscv-tests pass count. executor: `make bench-canonical-throughput` before/after.
     Anything touching execution semantics: `make diff` result. -->

```
$ just ...
```

## Purity

<!-- Name the PUR-N rules this change touches and say how each property still
     holds. Touching one is not a problem. Answer "None." if it touches none.
     PURITY.md states each rule in full and is the only place that does. -->

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
- [ ] Any PUR-N the author named still holds, verified by reading the diff
- [ ] For `executor`/`sqlcpu`: no perf regression >10% without an ADR justifying it
