#!/usr/bin/env python3
"""Standalone element-rate microbenchmarks for #257's two write-log costs.

## Why this exists as a separate instrument

The seeded sweep (`bench_l0.py`) measures how much a batch slows down per extra
write-log element. It cannot say WHICH of the two length-proportional costs is
responsible, because both scale with the same length and the fold offers no way
to vary one without the other -- `acc.3`'s three lanes are parallel arrays, so
seeding them to unequal lengths is a semantic change, not a cost-only one (see
`seed.py`, and the executed guard in
`executor/tests/test_fold.py::test_unequal_lane_seed_breaks_forwarding`).

So the split is done from OUTSIDE the fold: measure each primitive's own
per-element rate, predict what they jointly imply for the sweep's slope, and
compare. Two instruments sharing no assumption is a real anchor; a subtraction
between two arms of the same instrument is not (#197).

## The prediction

Per step, per seeded write-log element, the step expression pays:

  * SIX `arrayLastIndex` element-visits. `LW` contains two, and `LW` is
    textually expanded three times. (#180 §6 says "twice" -- that is the count
    inside one `LW` and undercounts by 3x.)
  * ONE 16-byte copy: `new_wl`'s three `arrayPushBack` calls on acc.3
    (UInt32 + UInt32 + UInt64), evaluated on EVERY step whether or not a store
    retires, because `if` does not short-circuit inside `arrayFold` (#183).

For a batch of K steps, the predicted slope in ms per seeded element is

    g_predicted = K * (6 * t_scan_elem + t_copy_elem) * 1000

## Verification, not assumption

A microbenchmark whose work got optimised away reads exactly like a fast one.
Both measurements here are therefore swept over array length and checked for
LINEARITY: real O(length) work produces a straight line through the length
axis, an elided computation produces a flat one. The fitted slope is the rate;
the R^2 and the flat-line check are what license quoting it. If either comes
back flat, this script reports the rate as UNSUPPORTED rather than returning a
number -- the attribution is then simply not available, and #257 reports the
combined cost only.

Constant folding is defeated by making each row's array and probe depend on
`number`, so ClickHouse cannot hoist the expression out of the scan.

Determinism (SPEC §8): nothing here reads a host clock or any randomness on a
path that affects a reported number. Durations come from
`system.query_log.query_duration_ms` keyed by `query_id`, i.e. the server's own
accounting, not a wall-clock difference.
"""
import argparse
import json
import subprocess
import uuid


class CH:
    def __init__(self, container, password, database="default"):
        self.base = ["docker", "exec", "-i", container, "clickhouse-client",
                     "--host", "localhost", "--port", "9000", "--user", "default",
                     "--database", database]
        if password:
            self.base += ["--password", password]

    def run(self, sql, query_id=None):
        qid = query_id or ("wl257_" + uuid.uuid4().hex)
        proc = subprocess.run(self.base + ["--query_id", qid],
                              input=sql, capture_output=True, text=True)
        if proc.returncode != 0:
            raise RuntimeError(f"query {qid} failed:\n{proc.stderr[-3000:]}")
        return proc.stdout.strip(), qid

    def duration_ms(self, qid):
        self.run("SYSTEM FLUSH LOGS")
        out, _ = self.run(
            "SELECT query_duration_ms FROM system.query_log "
            f"WHERE query_id = '{qid}' AND type = 'QueryFinish' FORMAT TSV")
        return int(out.splitlines()[0])


# `arr` and the probe value both depend on `number`, so neither the array
# construction nor the scan can be hoisted out of the row loop. The probe is
# UInt32::MAX - number, which never matches the fill (UInt32::MAX) except at
# number = 0 -- so all but one row scans the full array without an early match.
# `arrayLastIndex` seeks the LAST match and cannot short-circuit on a miss
# anyway, which is the property that makes this the right model of the fold's
# behaviour on a seeded log.
SCAN_SQL = """SELECT sum(arrayLastIndex(z -> z = probe, arr)) AS s
FROM (SELECT arrayResize(emptyArrayUInt32(), {L}, toUInt32(4294967295 - number % 3)) AS arr,
             toUInt32(4294967290 - number) AS probe
      FROM numbers({rows}))
SETTINGS max_threads = 1"""

# The accumulator copy, measured INSIDE arrayFold on acc.3's exact three-lane
# shape (UInt32 + UInt32 + UInt64), grown exactly as the real step expression
# grows it.
#
# An earlier version measured this outside a fold, forcing materialisation with
# `cityHash64` over the three pushed arrays. That was wrong by a factor of 30:
# the hash is itself O(length) and dominated the measurement, reporting
# 18.5 ns/element when the copy is 0.63. A microbenchmark's scaffolding has to
# be cheaper than the thing it is scaffolding, and that one was not.
#
# Growing the array inside the fold needs no scaffolding at all -- the copies
# are the work, and `length(...).1` at the end is O(1). Over `range(N)` the
# accumulator is copied at lengths 0, 1, ... N-1, so the total is N*(N-1)/2
# element-copies. That quadratic is also the H1 answer: an in-place
# implementation would be linear.
COPY_FOLD_SQL = """SELECT length(arrayFold(
    (acc, i) -> tuple(arrayPushBack(acc.1, i),
                      arrayPushBack(acc.2, i),
                      arrayPushBack(acc.3, toUInt64(i))),
    range({N}),
    tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64())).1) AS n
SETTINGS max_threads = 1"""

# The same shape with L held at 0, to price the per-row scaffolding (array
# construction, hashing, the numbers() scan) that the slope must not include.
# It is the intercept, and it is measured rather than fitted away.


# --- direct tests of the two mechanism hypotheses -------------------------
# Comparing the sweep's slope against the primitive rates can only say the
# fold is cheaper than a naive reading of its text implies. It cannot say
# WHICH of the two candidate reasons is responsible, because both would
# produce the same shortfall. These probes test each hypothesis directly, so
# the attribution is observed rather than inferred.
#
# H1 -- does `arrayPushBack` inside `arrayFold` copy the accumulator, or mutate
# it in place? -- is answered by COPY_FOLD_SQL's growth exponent above, which
# is the same sweep that supplies the copy rate. A copying implementation is
# O(N^2), an in-place one O(N), and the exponent must be read at the LARGE end:
# at N <= 80,000 it reads ~1.4, which is ambiguous, and only by N = 320,000
# does it reach ~1.9. An earlier version stopped at 80,000 and called 1.43
# "quadratic", which the data did not support.

# H2: does ClickHouse collapse the six textually-repeated `arrayLastIndex`
# calls into one? Two folds over the same data, one evaluating the call once
# per step and one evaluating the identical call six times. If common
# subexpression elimination is doing the work, they cost the same; if not, the
# six-call variant costs about six times the scan.
#
# The six terms are byte-identical, which is exactly the condition under which
# CSE can fire -- and exactly the situation `LW`'s triple textual expansion
# creates in the real step expression.
_LASTIDX = "arrayLastIndex(z -> z = toUInt32(i), arr)"
SCAN_CSE_SQL = """SELECT arrayFold((acc, i) -> acc + toUInt64({terms}),
                                   range({N}), toUInt64(0)) AS s
FROM (SELECT arrayResize(emptyArrayUInt32(), {L}, toUInt32(4294967295)) AS arr)
SETTINGS max_threads = 1"""


def sweep(ch, sql_tmpl, lengths, rows, reps):
    """Median server-side ms for each array length."""
    out = {}
    for L in lengths:
        runs = []
        for _ in range(reps):
            _, qid = ch.run(sql_tmpl.format(L=L, rows=rows))
            runs.append(ch.duration_ms(qid))
        runs.sort()
        out[L] = runs[len(runs) // 2]
    return out


def ols(xs, ys):
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    sxy = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    slope = sxy / sxx
    intercept = my - slope * mx
    ss_tot = sum((y - my) ** 2 for y in ys)
    ss_res = sum((y - (intercept + slope * x)) ** 2 for x, y in zip(xs, ys))
    r2 = 1.0 - (ss_res / ss_tot) if ss_tot > 0 else 0.0
    return slope, intercept, r2


def rate(name, points, rows):
    """Seconds per element-operation, with the evidence that licenses quoting it.

    `points` maps array length -> median ms for `rows` rows. The slope of
    ms-vs-length, divided by `rows`, is ms per element per row.
    """
    xs = sorted(points)
    ys = [points[x] for x in xs]
    slope_ms_per_elem, intercept_ms, r2 = ols(xs, ys)
    per_elem_s = (slope_ms_per_elem / rows) / 1000.0
    # A flat line means the work was elided or is dominated by scaffolding.
    span = max(ys) - min(ys)
    supported = r2 >= 0.95 and span > 0.25 * max(ys) and slope_ms_per_elem > 0
    return {
        "name": name,
        "points_ms": {str(k): v for k, v in points.items()},
        "rows": rows,
        "slope_ms_per_element_total": slope_ms_per_elem,
        "intercept_ms": intercept_ms,
        "r2": r2,
        "seconds_per_element": per_elem_s,
        "ns_per_element": per_elem_s * 1e9,
        "supported": supported,
        "why_not": None if supported else (
            f"r2={r2:.3f}, span={span}ms over max={max(ys)}ms -- the curve is not "
            "convincingly linear in array length, so the work may have been "
            "optimised away rather than measured. Attribution UNSUPPORTED."),
    }


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--container", default="clickdoom-ch")
    p.add_argument("--password", default="clickdoom")
    p.add_argument("--lengths", default="0,5000,10000,20000,40000,80000")
    # The scan is roughly 15x cheaper per element than the copy, so a row count
    # that makes the copy take seconds leaves the scan down in the tens of
    # milliseconds, where scheduler noise swamps the slope. (Measured: at
    # rows=2000 the scan curve came back non-monotonic and this script's own
    # linearity gate refused it -- correctly.) They therefore get separate row
    # counts, chosen so both land in the same ~1-3 s band.
    p.add_argument("--scan-rows", type=int, default=20000)
    # The copy sweep must reach far enough for the growth exponent to separate
    # O(N) from O(N^2): at N <= 80,000 it reads ~1.4, which is ambiguous, and
    # only by 320,000 does it reach ~1.9.
    p.add_argument("--copy-n", default="40000,80000,160000,320000")
    p.add_argument("--reps", type=int, default=5)
    p.add_argument("--k", type=int, default=60000,
                   help="batch length the prediction is expressed for")
    p.add_argument("--out", default=None)
    a = p.parse_args()

    ch = CH(a.container, a.password)
    lengths = [int(x) for x in a.lengths.split(",")]

    import math

    scan = rate("arrayLastIndex",
                sweep(ch, SCAN_SQL, lengths, a.scan_rows, a.reps), a.scan_rows)

    # The copy rate and H1's exponent come from ONE sweep: growing acc.3 inside
    # arrayFold over range(N) performs N*(N-1)/2 element-copies, so the rate is
    # t / (N*(N-1)/2) and the exponent says whether copying happens at all.
    growth = {}
    for n in [int(x) for x in a.copy_n.split(",")]:
        runs = sorted(ch.duration_ms(ch.run(COPY_FOLD_SQL.format(N=n))[1])
                      for _ in range(a.reps))
        growth[n] = runs[len(runs) // 2]
    ns = sorted(growth)
    # t ~ N^p  =>  p = dlog(t)/dlog(N), taken over the LARGEST pair: the small-N
    # points carry per-query overhead that biases the exponent down, and it is
    # the asymptote that distinguishes O(N) from O(N^2).
    exponent = (math.log(growth[ns[-1]] / growth[ns[-2]])
                / math.log(ns[-1] / ns[-2])) if growth[ns[-2]] > 0 else float("nan")
    n_big = ns[-1]
    copy_ns = 1e6 * growth[n_big] / (n_big * (n_big - 1) / 2)
    copy = {"name": "acc.3 3-lane copy, in arrayFold", "points_ms": growth,
            "ns_per_element": copy_ns, "exponent": exponent,
            "seconds_per_element": copy_ns / 1e9,
            "supported": exponent > 1.7,
            "why_not": None if exponent > 1.7 else (
                f"growth exponent {exponent:.2f} is not clearly quadratic; the "
                f"copy model (t = rate * N^2/2) does not hold, so this rate is "
                f"UNSUPPORTED")}

    # H2: does CSE collapse the repeated scan?
    cse = {}
    for label, terms in (("x1", _LASTIDX),
                         ("x6", " + ".join([_LASTIDX] * 6))):
        runs = sorted(ch.duration_ms(
            ch.run(SCAN_CSE_SQL.format(terms=terms, N=20000, L=20000))[1])
            for _ in range(a.reps))
        cse[label] = runs[len(runs) // 2]
    cse_ratio = cse["x6"] / cse["x1"] if cse["x1"] else float("nan")

    result = {"scan": scan, "copy": copy, "k": a.k,
              "scan_multiplicity_textual": 6,
              "cse_probe_ms": cse,
              "cse_ratio_x6_over_x1": cse_ratio,
              "cse_collapses": cse_ratio < 2.0,
              "clickhouse_version": ch.run("SELECT version()")[0]}

    if scan["supported"] and copy["supported"]:
        # H2 decides the scan multiplicity the fold actually pays. The six
        # textual calls are byte-identical, so if CSE collapses them the fold
        # pays for one -- measured, not assumed.
        mult = 1 if result["cse_collapses"] else 6
        result["scan_multiplicity_paid"] = mult
        per_step_ns = mult * scan["ns_per_element"] + copy["ns_per_element"]
        result["predicted_ns_per_element_per_step"] = per_step_ns
        result["predicted_g_VA_ms_per_seeded_element"] = (
            a.k * per_step_ns / 1e6)
        result["predicted_scan_share"] = (
            mult * scan["ns_per_element"] / per_step_ns)
    else:
        result["scan_multiplicity_paid"] = None
        result["predicted_ns_per_element_per_step"] = None
        result["predicted_g_VA_ms_per_seeded_element"] = None
        result["predicted_scan_share"] = None

    print(json.dumps(result, indent=2))
    if a.out:
        with open(a.out, "w") as f:
            json.dump(result, f, indent=2)

    print()
    print(f"{scan['name']:<34} {scan['ns_per_element']:8.3f} ns/element  "
          f"r2={scan['r2']:.4f}  "
          f"[{'OK' if scan['supported'] else 'UNSUPPORTED'}]")
    if not scan["supported"]:
        print(f"    {scan['why_not']}")
    print(f"{copy['name']:<34} {copy['ns_per_element']:8.3f} ns/element  "
          f"exponent={copy['exponent']:.2f}  "
          f"[{'OK' if copy['supported'] else 'UNSUPPORTED'}]")
    if not copy["supported"]:
        print(f"    {copy['why_not']}")

    print("\n--- direct mechanism probes ---")
    print(f"H1 acc.3 copy growth inside arrayFold: {growth}")
    print(f"   exponent = {copy['exponent']:.2f} -> "
          + ("O(N^2): the accumulator IS copied every step"
             if copy["exponent"] > 1.7 else
             "sub-quadratic: copying is not the model, treat the rate as void"))
    print(f"H2 repeated arrayLastIndex: x1={cse['x1']} ms, x6={cse['x6']} ms, "
          f"ratio {cse_ratio:.2f}x")
    print("   -> " + ("CSE collapses the repeats: 6 textual calls cost ~1"
                      if result["cse_collapses"] else
                      "no CSE: each textual repeat is evaluated"))

    if result["predicted_ns_per_element_per_step"] is not None:
        m = result["scan_multiplicity_paid"]
        print("\npredicted fold cost, per write-log element per step:")
        print(f"   {m} x scan {scan['ns_per_element']:.3f} + copy "
              f"{copy['ns_per_element']:.3f} = "
              f"{result['predicted_ns_per_element_per_step']:.3f} ns")
        print(f"   scan is {100 * result['predicted_scan_share']:.0f}% of it")
        print(f"   (= {result['predicted_g_VA_ms_per_seeded_element']:.4f} ms "
              f"per seeded element at K={a.k:,})")


if __name__ == "__main__":
    main()
