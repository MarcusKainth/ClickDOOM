#!/usr/bin/env python3
"""Summarise arms produced by `bench.py`, and fit #180's `T(K) = S + c*K`
from a fixed-instruction-window K-sweep (`ksweep.sh`).

The fit deliberately does NOT least-squares four points of a two-parameter
model and report one number with no spread. `ksweep.sh` arranges every arm
to execute the same instruction window, so successive arms differ by a
known number of setups and each adjacent pair yields its OWN estimate of
`S`. Those estimates are printed individually. Their spread is the error
bar, and it is an honest one: it comes from independent measurements, not
from a residual.

Usage:
    fit.py /tmp/sq2-bench/*.json
"""
import json
import sys


def load(paths):
    out = []
    for p in paths:
        with open(p) as f:
            out.append(json.load(f))
    return out


def srv_ms(stmt):
    return stmt["server"]["query_duration_ms"] if stmt.get("server") else None


def summarise(d):
    rows = []
    for b in d["batches"]:
        row = {"batch": b["batch"], "retired": b["retired"], "halted": b["halted"]}
        for name, st in b["statements"].items():
            row[name] = srv_ms(st)
            row[name + "_wall"] = round(st["wall_s"], 3)
        row["compile_function"] = b["statements"]["fold"]["server"]["compile_function"]
        rows.append(row)
    return rows


def main():
    paths = sys.argv[1:]
    if not paths:
        sys.exit(__doc__)
    arms = load(paths)

    print("== per-arm summary (server-side query_duration_ms) ==")
    sweep = {}
    for d in arms:
        rows = summarise(d)
        fold_total = sum(r["fold"] for r in rows)
        retired_total = sum(r["retired"] for r in rows)
        ramt = [r["server"]["query_duration_ms"] for r in d["ramt_standalone"]]
        print(f"\n-- {d['label']}  K={d['k']} batches={len(rows)} "
              f"retired_total={retired_total} ram_parts_at_start={d['ram_active_parts_at_start']}")
        print(f"   RAMT standalone ms: {ramt}")
        for r in rows:
            stmts = " ".join(f"{k}={r[k]}" for k in
                             ("fold", "ram", "console_out", "cpu_state", "retention") if r.get(k) is not None)
            print(f"   b{r['batch']:<3} retired={r['retired']:<7} {stmts} "
                  f"CompileFunction={r['compile_function']}")
        print(f"   fold total = {fold_total} ms over {retired_total} retired instructions")
        sweep.setdefault(d["k"], []).append((len(rows), fold_total, retired_total))

    # --- #180 fit, from adjacent arms of the fixed-window sweep ----------
    ks = sorted(k for k in sweep if k > 1)
    usable = []
    for k in ks:
        nb, total, retired = sweep[k][0]
        if retired != nb * k:
            print(f"\n!! K={k}: retired {retired} != batches*K {nb * k} -- HWM truncation or a "
                  f"halt; this arm does NOT cover the same instruction window and is excluded "
                  f"from the fit.")
            continue
        usable.append((k, nb, total, retired))
    if len(usable) >= 2:
        print("\n== #180 fit: S from adjacent fixed-window arms ==")
        ests = []
        for (k_lo, nb_lo, t_lo, _), (k_hi, nb_hi, t_hi, _) in zip(usable, usable[1:]):
            d_batches = nb_lo - nb_hi
            if d_batches <= 0:
                continue
            s = (t_lo - t_hi) / d_batches
            ests.append(s)
            print(f"   K={k_lo} ({nb_lo} batches, {t_lo} ms) vs K={k_hi} ({nb_hi} batches, "
                  f"{t_hi} ms): S = {t_lo - t_hi} / {d_batches} = {s:.0f} ms")
        if ests:
            lo, hi = min(ests), max(ests)
            mean = sum(ests) / len(ests)
            print(f"   S = {mean:.0f} ms  (independent estimates span {lo:.0f}..{hi:.0f} ms)")
            k_ref, nb_ref, t_ref, _ = usable[-1]
            per_batch = t_ref / nb_ref
            print(f"   at K={k_ref}: S is {100 * mean / per_batch:.1f}% of a {per_batch:.0f} ms fold")
    if 1 in sweep:
        nb, total, _ = sweep[1][0]
        print(f"\n== direct intercept: K=1 fold = {total / nb:.0f} ms mean over {nb} batches ==")


if __name__ == "__main__":
    main()
