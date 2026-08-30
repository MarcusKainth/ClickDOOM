#!/usr/bin/env python3
"""Fit #257's per-element write-log cost from one or more `bench_l0.py` runs.

Reports the slope `g = d(fold_ms)/d(L0)` -- milliseconds of extra fold time per
extra write-log element carried through a batch -- with a confidence interval,
and reconciles it against the two independent predictions:

  * `micro.py`'s primitive rates (6 * arrayLastIndex + one 16-byte copy);
  * #180's fitted `beta * W = 0.0916 ms` per unit of K, which at K = 60,000
    implies about 0.275 ms per element.

## Estimator choice

**Theil-Sen** is the point estimate: the median of all pairwise slopes. One bad
arm -- a background process landing on the box for thirty seconds -- moves a
least-squares slope and does not move a median of pairwise slopes. OLS and R^2
are reported alongside so the two can be compared, and a large gap between them
is itself information (it means an outlier is present, not that the fit is
subtly better).

**Percentile bootstrap** over repeats within each L0 gives the interval,
resampling whole repeats rather than residuals so no normality is assumed.

**Curvature is reported, not smoothed.** A linear scan predicts a straight
line. Residuals are printed per L0; systematic curvature would mean the cost is
not O(length) and the model is wrong, which is a finding rather than noise.

## The per-instruction claim

`slope / K` is the per-element, per-step cost. A linear scan predicts it is
CONSTANT across K -- that constancy, checked across several K values, is the
mix-free answer to "does per-instruction cost grow within a batch", and it does
not depend on any fit. Pass several runs at different K to see it.
"""
import argparse
import json
import statistics
import sys


def load(paths):
    runs = []
    for p in paths:
        with open(p) as f:
            runs.append(json.load(f))
    return runs


def net_by_l0(run):
    """L0 -> list of net (fold-only) ms, one per repeat."""
    out = {}
    for r in run["records"]:
        if r["k"] == run["k"] and r["label"].startswith("L") and "net_ms" in r:
            out.setdefault(r["l0"], []).append(r["net_ms"])
    return out


def theil_sen(pts):
    slopes = []
    for i in range(len(pts)):
        for j in range(i + 1, len(pts)):
            (x1, y1), (x2, y2) = pts[i], pts[j]
            if x1 != x2:
                slopes.append((y2 - y1) / (x2 - x1))
    if not slopes:
        return None, None
    slope = statistics.median(slopes)
    intercept = statistics.median(y - slope * x for x, y in pts)
    return slope, intercept


def ols(pts):
    n = len(pts)
    mx = sum(x for x, _ in pts) / n
    my = sum(y for _, y in pts) / n
    sxx = sum((x - mx) ** 2 for x, _ in pts)
    if sxx == 0:
        return None, None, None
    slope = sum((x - mx) * (y - my) for x, y in pts) / sxx
    intercept = my - slope * mx
    ss_tot = sum((y - my) ** 2 for _, y in pts)
    ss_res = sum((y - (intercept + slope * x)) ** 2 for x, y in pts)
    r2 = 1 - ss_res / ss_tot if ss_tot else float("nan")
    return slope, intercept, r2


def bootstrap_ci(by_l0, b=10000, seed_val=12345):
    """Percentile CI on the Theil-Sen slope, resampling repeats within each L0.

    Deliberately seeded and deliberately NOT using `random` seeded from a
    clock: the reported interval must be reproducible from the input JSON
    alone (SPEC 8), so the same data always yields the same interval.
    """
    state = seed_val
    def rnd(n):
        nonlocal state
        state = (state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return (state >> 33) % n

    l0s = sorted(by_l0)
    slopes = []
    for _ in range(b):
        pts = []
        for l0 in l0s:
            vals = by_l0[l0]
            pts.append((l0, vals[rnd(len(vals))]))
        s, _ = theil_sen(pts)
        if s is not None:
            slopes.append(s)
    slopes.sort()
    lo = slopes[int(0.025 * len(slopes))]
    hi = slopes[int(0.975 * len(slopes))]
    return lo, hi


# #180 fitted `Total(K) = (W/K)*S + a*W + beta*W*K` over a fixed window of
# W = 120,000 instructions, and reported `beta*W = 0.0916` ms per unit of K.
BETA_W_180 = 0.0916      # ms per unit of K, for the whole window
W_180 = 120_000          # instructions in that window

# Boot's general-RAM store density, measured on #180: a K = 80,000 batch
# retired 60,006 before the write-log hit HWM = 20,000, i.e. 20,000 pushes in
# 60,006 steps. This is the conversion factor between "cost per batch" and
# "cost per element per step", so it is named and sourced rather than assumed.
RHO_STORE_BOOT = 20_000 / 60_006


def per_element_per_step_from_180(rho):
    """#180's beta, converted to nanoseconds per write-log element per step.

    #180's model prices the whole window. Per batch, its write-log term is

        beta*W * K / (W/K)  =  beta*W * K^2 / W        ms

    and that is paid over K steps against a log that grows linearly from 0 to
    rho*K, i.e. a MEAN length of rho*K/2. So

        cost per (step * element) = (beta*W * K^2 / W) / (K * rho*K/2)
                                  = 2 * beta*W / (W * rho)

    -- independent of K, which is the same invariance this harness measures
    directly. That the two agree at all is the reconciliation; that this
    expression has no K in it is why the comparison is legitimate.
    """
    return 1e6 * 2 * BETA_W_180 / (W_180 * rho)   # ms -> ns


def main():
    p = argparse.ArgumentParser()
    p.add_argument("json", nargs="+")
    p.add_argument("--micro", default=None, help="micro.py output, for attribution")
    p.add_argument("--rho-store", type=float, default=RHO_STORE_BOOT,
                   help="general-RAM store density of the window #180 measured; "
                        "converts its per-window fit into a per-element rate")
    a = p.parse_args()

    runs = load(a.json)
    micro = json.load(open(a.micro)) if a.micro else None

    per_k = {}
    for run in runs:
        if run.get("aborted"):
            print(f"!! {run['db']} K={run['k']}: ABORTED -- {run['aborted']}\n"
                  f"   partial data below, treat as indicative only", file=sys.stderr)
        by = net_by_l0(run)
        if len(by) < 2:
            print(f"!! K={run['k']}: only {len(by)} L0 point(s), cannot fit",
                  file=sys.stderr)
            continue
        med = [(l0, statistics.median(v)) for l0, v in sorted(by.items())]
        ts_slope, ts_int = theil_sen(med)
        ols_slope, ols_int, r2 = ols(med)
        lo, hi = bootstrap_ci(by)
        per_k[run["k"]] = {
            "run": run, "medians": med, "theil_sen": ts_slope,
            "intercept": ts_int, "ols": ols_slope, "r2": r2,
            "ci95": (lo, hi), "n_reps": {l0: len(v) for l0, v in by.items()},
        }

    for k in sorted(per_k):
        f = per_k[k]
        print(f"\n=== K = {k:,}  db={f['run']['db']}  hwm={f['run']['hwm']:,} "
              f"CH={f['run']['clickhouse_version']} ===")
        print(f"{'L0':>8} {'reps':>5} {'median net ms':>14} {'residual ms':>12}")
        for l0, m in f["medians"]:
            resid = m - (f["intercept"] + f["theil_sen"] * l0)
            print(f"{l0:>8,} {f['n_reps'][l0]:>5} {m:>14,.0f} {resid:>12,.1f}")
        print(f"  Theil-Sen slope : {f['theil_sen']:.6f} ms per seeded element")
        print(f"  95% bootstrap CI: [{f['ci95'][0]:.6f}, {f['ci95'][1]:.6f}]")
        print(f"  OLS slope       : {f['ols']:.6f}   R^2 = {f['r2']:.5f}")
        print(f"  per element per step (slope/K): {1e6 * f['theil_sen'] / k:.4f} ns")

    # The headline: slope/K constant across K is the mix-free statement that
    # per-instruction cost grows with write-log length.
    if len(per_k) >= 2:
        print("\n=== slope/K across K -- a linear scan predicts a CONSTANT ===")
        vals = []
        for k in sorted(per_k):
            v = 1e6 * per_k[k]["theil_sen"] / k
            vals.append(v)
            print(f"  K = {k:>7,}: {v:.4f} ns per element per step")
        spread = (max(vals) - min(vals)) / statistics.mean(vals)
        print(f"  spread = {100 * spread:.1f}% of the mean")
        print("  -> CONSISTENT with a per-step cost linear in write-log length"
              if spread < 0.25 else
              "  -> NOT constant; the cost is not simply linear in K, report as such")

    # Reconciliation. Everything is expressed as ns per write-log element per
    # step, which is the only unit in which all three sources are comparable:
    # #180 prices a window, micro.py prices a primitive, this harness prices a
    # seeded element.
    ref_k = max(per_k) if per_k else None
    if ref_k:
        g_ns = 1e6 * per_k[ref_k]["theil_sen"] / ref_k
        lo_ns = 1e6 * per_k[ref_k]["ci95"][0] / ref_k
        hi_ns = 1e6 * per_k[ref_k]["ci95"][1] / ref_k
        print("\n=== reconciliation, ns per write-log element per step ===")
        print(f"  measured here (K={ref_k:,})     : {g_ns:.3f}  "
              f"95% CI [{lo_ns:.3f}, {hi_ns:.3f}]")

        n180 = per_element_per_step_from_180(a.rho_store)
        print(f"  #180's beta implies          : {n180:.3f}   "
              f"(measured / #180 = {g_ns / n180:.2f}x, "
              f"rho_store = {a.rho_store:.4f})")

        if micro and micro["scan"]["supported"] and micro["copy"]["supported"]:
            s = micro["scan"]["ns_per_element"]
            c = micro["copy"]["ns_per_element"]
            mult = micro["scan_multiplicity_paid"]
            pred = micro["predicted_ns_per_element_per_step"]
            print("\n=== attribution, from separately measured primitives ===")
            print(f"  scan  {s:6.3f} ns/element   x{mult} paid "
                  f"(6 in the text; CSE probe ratio "
                  f"{micro['cse_ratio_x6_over_x1']:.2f}x)")
            print(f"  copy  {c:6.3f} ns/element   acc.3's three lanes, "
                  f"growth exponent {micro['copy']['exponent']:.2f}")
            print(f"  ----- {pred:6.3f} ns/element/step predicted   "
                  f"vs {g_ns:.3f} measured  ({g_ns / pred:.2f}x)")
            share = micro["predicted_scan_share"]
            print(f"\n  The load-forwarding SCAN is {100 * share:.0f}% of the "
                  f"write-log term;\n  the accumulator copy is "
                  f"{100 * (1 - share):.0f}%.")
            if mult == 1:
                print("\n  NOTE: ClickHouse already collapses the six textual\n"
                      "  arrayLastIndex calls into one (measured, and consistent\n"
                      "  with #191's null result on the double scan). So binding\n"
                      "  the scan once by hand -- #180 6's proposal -- has NO\n"
                      "  headroom: the optimiser has already done it.")


if __name__ == "__main__":
    main()
