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

# The fold copies three lanes per step: UInt32 + UInt32 + UInt64 = 16 bytes per
# element. Modelled with the same three pushBacks rather than one, so the
# measured rate is per-ELEMENT-of-write-log and directly comparable to the
# sweep's slope without a bytes-per-element conversion in between.
#
# `cityHash64` over the three results forces each pushed array to be
# materialised: a `length()` reduction would let ClickHouse answer `L + 1`
# without ever copying, which is exactly the elision the linearity check exists
# to catch. Verified by that check, not assumed.
COPY_SQL = """SELECT sum(cityHash64(arrayPushBack(a32, toUInt32(n)),
                                    arrayPushBack(b32, toUInt32(n)),
                                    arrayPushBack(c64, toUInt64(n)))) AS s
FROM (SELECT number AS n,
             arrayResize(emptyArrayUInt32(), {L}, toUInt32(number)) AS a32,
             arrayResize(emptyArrayUInt32(), {L}, toUInt32(number + 1)) AS b32,
             arrayResize(emptyArrayUInt64(), {L}, toUInt64(number + 2)) AS c64
      FROM numbers({rows}))
SETTINGS max_threads = 1"""

# The same shape with L held at 0, to price the per-row scaffolding (array
# construction, hashing, the numbers() scan) that the slope must not include.
# It is the intercept, and it is measured rather than fitted away.


# --- direct tests of the two mechanism hypotheses -------------------------
# Comparing the sweep's slope against the primitive rates can only say the
# fold is cheaper than a naive reading of its text implies. It cannot say
# WHICH of the two candidate reasons is responsible, because both would
# produce the same shortfall. These two probes test each hypothesis directly,
# outside the fold, so the attribution is observed rather than inferred.

# H1: does `arrayPushBack` inside `arrayFold` copy the accumulator, or mutate
# it in place? A copying implementation makes this O(N^2); an in-place one
# makes it O(N). Sweep N and read the exponent off a log-log fit -- the answer
# is the exponent, and it is unambiguous (1 vs 2, not a subtle ratio).
#
# `length()` of the result, not the array itself, so the measurement is the
# fold and not the serialisation of an N-element array.
PUSHBACK_GROWTH_SQL = """SELECT length(arrayFold(
    (acc, i) -> arrayPushBack(acc, i), range({N}), emptyArrayUInt32())) AS n
SETTINGS max_threads = 1"""

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
    p.add_argument("--copy-rows", type=int, default=2000)
    p.add_argument("--reps", type=int, default=5)
    p.add_argument("--k", type=int, default=60000,
                   help="batch length the prediction is expressed for")
    p.add_argument("--out", default=None)
    a = p.parse_args()

    ch = CH(a.container, a.password)
    lengths = [int(x) for x in a.lengths.split(",")]

    scan = rate("arrayLastIndex",
                sweep(ch, SCAN_SQL, lengths, a.scan_rows, a.reps), a.scan_rows)
    copy = rate("arrayPushBack x3 (16B/elem)",
                sweep(ch, COPY_SQL, lengths, a.copy_rows, a.reps), a.copy_rows)

    # H1: accumulator copy semantics, from the growth exponent.
    growth = {}
    for n in (10000, 20000, 40000, 80000):
        runs = sorted(ch.duration_ms(ch.run(PUSHBACK_GROWTH_SQL.format(N=n))[1])
                      for _ in range(a.reps))
        growth[n] = runs[len(runs) // 2]
    ns = sorted(growth)
    # Exponent from the first and last point: t ~ N^p  =>  p = dlog(t)/dlog(N).
    import math
    if growth[ns[0]] > 0:
        exponent = (math.log(growth[ns[-1]] / growth[ns[0]])
                    / math.log(ns[-1] / ns[0]))
    else:
        exponent = float("nan")

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
              "scan_multiplicity": 6,
              "pushback_growth_ms": growth,
              "pushback_growth_exponent": exponent,
              "cse_probe_ms": cse,
              "cse_ratio_x6_over_x1": cse_ratio,
              "clickhouse_version": ch.run("SELECT version()")[0]}

    if scan["supported"] and copy["supported"]:
        g_ms_per_elem = a.k * (6 * scan["seconds_per_element"]
                               + copy["seconds_per_element"]) * 1000.0
        result["predicted_g_VA_ms_per_seeded_element"] = g_ms_per_elem
        result["predicted_scan_share"] = (
            6 * scan["seconds_per_element"]
            / (6 * scan["seconds_per_element"] + copy["seconds_per_element"]))
    else:
        result["predicted_g_VA_ms_per_seeded_element"] = None
        result["predicted_scan_share"] = None

    print(json.dumps(result, indent=2))
    if a.out:
        with open(a.out, "w") as f:
            json.dump(result, f, indent=2)

    print()
    for r in (scan, copy):
        status = "OK" if r["supported"] else "UNSUPPORTED"
        print(f"{r['name']:<32} {r['ns_per_element']:8.3f} ns/element  "
              f"r2={r['r2']:.4f}  [{status}]")
        if not r["supported"]:
            print(f"    {r['why_not']}")
    if result["predicted_g_VA_ms_per_seeded_element"] is not None:
        print(f"\npredicted slope at K={a.k}: "
              f"{result['predicted_g_VA_ms_per_seeded_element']:.4f} ms "
              f"per seeded write-log element "
              f"(scan {100 * result['predicted_scan_share']:.0f}% / "
              f"copy {100 * (1 - result['predicted_scan_share']):.0f}%)")

    print("\n--- direct mechanism probes ---")
    print(f"H1 arrayPushBack inside arrayFold: {growth}")
    print(f"   growth exponent = {exponent:.2f}  "
          f"({'O(N) -- accumulator mutated IN PLACE, no per-step copy'
              if exponent < 1.4 else
              'O(N^2) -- accumulator COPIED every step'})")
    print(f"H2 repeated arrayLastIndex: x1={cse['x1']} ms, x6={cse['x6']} ms, "
          f"ratio {cse_ratio:.2f}x")
    print(f"   ({'CSE collapses the repeats -- 6 textual calls cost ~1'
             if cse_ratio < 2.0 else
             'each textual repeat is evaluated -- no CSE'})")


if __name__ == "__main__":
    main()
