#!/usr/bin/env python3
"""Emit the B2 microbenchmark queries: what does an unselected branch cost inside arrayFold?

Each variant folds K steps over a two-field UInt64 accumulator. CHAIN(x) is L links of
bitXor(plus(x, c1), c2), so 2*L function nodes with distinct constants. COND is false on
every step but depends on the accumulator, so it cannot be constant-folded.

Usage: gen.py [--k 20000] [--links 100] [--salt 0] [--list | variant...]
Prints one query per requested variant as: name<TAB>sql
"""
import argparse
import hashlib

def chain(x, links, salt, tag):
    expr = x
    for k in range(links):
        h = hashlib.sha256(f"{salt}:{tag}:{k}".encode()).digest()
        c1 = int.from_bytes(h[:4], "little") | 1
        c2 = int.from_bytes(h[4:8], "little") | 1
        expr = f"bitXor(plus({expr}, toUInt64({c1})), toUInt64({c2}))"
    return expr

COND = "(acc.2 > toUInt64(9000000000))"          # never true: acc.2 counts steps

def fault_link(x, cond):
    # intDiv by zero if the branch is evaluated while cond is false
    return f"plus({x}, intDiv({x}, toUInt64({cond})))"

def fold(step, k):
    return (f"SELECT arrayFold((acc, i) -> {step}, range({k}), "
            f"tuple(toUInt64(1), toUInt64(0))).1 AS v")

def variants(k, links, salt):
    v = {}
    v["floor"] = fold("tuple(acc.1 + 1, acc.2 + 1)", k)
    v["chain"] = fold(f"tuple({chain('acc.1', links, salt, 'chain')}, acc.2 + 1)", k)
    v["if_guarded"] = fold(f"tuple(if({COND}, {chain('acc.1', links, salt, 'if')}, acc.1 + 1), acc.2 + 1)", k)
    v["multiif_guarded"] = fold(f"tuple(multiIf({COND}, {chain('acc.1', links, salt, 'mif')}, acc.1 + 1), acc.2 + 1)", k)
    v["arraymap_guarded"] = fold(
        f"tuple(acc.1 + 1 + arraySum(arrayMap(x -> {chain('x', links, salt, 'am')}, "
        f"if({COND}, [acc.1], emptyArrayUInt64()))), acc.2 + 1)", k)
    v["arrayfold_guarded"] = fold(
        f"tuple(arrayFold((a, x) -> {chain('a', links, salt, 'af')}, range(toUInt64({COND})), acc.1) + 1, acc.2 + 1)", k)
    # N guarded blocks per step, arrayMap shape, links/2 each: per-block dispatch cost
    for n in (10, 50):
        blocks = " + ".join(
            f"arraySum(arrayMap(x -> {chain('x', max(links // 2, 1), salt, f'amn{n}_{b}')}, "
            f"if((acc.2 > toUInt64({9000000000 + b})), [acc.1], emptyArrayUInt64())))"
            for b in range(n))
        v[f"arraymap_{n}blocks"] = fold(f"tuple(acc.1 + 1 + {blocks}, acc.2 + 1)", k)
    # fault probes: the guarded body divides by toUInt64(COND) = 0 if it is evaluated
    def fc(tag):
        return fault_link(chain('acc.1', 4, salt, tag), COND)
    v["fault_if"] = fold(f"tuple(if({COND}, {fc('fi')}, acc.1 + 1), acc.2 + 1)", 100)
    v["fault_multiif"] = fold(f"tuple(multiIf({COND}, {fc('fm')}, acc.1 + 1), acc.2 + 1)", 100)
    v["fault_arraymap"] = fold(
        f"tuple(acc.1 + 1 + arraySum(arrayMap(x -> {fault_link(chain('x', 4, salt, 'fa'), COND)}, "
        f"if({COND}, [acc.1], emptyArrayUInt64()))), acc.2 + 1)", 100)
    v["fault_arrayfold"] = fold(
        f"tuple(arrayFold((a, x) -> {fault_link(chain('a', 4, salt, 'ff'), COND)}, range(toUInt64({COND})), acc.1) + 1, acc.2 + 1)", 100)
    return v

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--k", type=int, default=20000)
    ap.add_argument("--links", type=int, default=100)
    ap.add_argument("--salt", default="0")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("names", nargs="*")
    a = ap.parse_args()
    v = variants(a.k, a.links, a.salt)
    if a.list:
        print("\n".join(v))
        return
    for name in (a.names or v):
        print(f"{name}\t{v[name]}")

if __name__ == "__main__":
    main()
