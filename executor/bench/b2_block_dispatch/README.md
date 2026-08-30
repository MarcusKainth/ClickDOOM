# B2: what does an unselected branch cost inside arrayFold?

Static block translation only pays if a block that is not selected costs close to nothing per step.
This measures that on the repo pin, ClickHouse 26.7.5.10, fresh container, `max_threads=1`, `compile_expressions=0`.

K=20,000 steps. The body is a 200-node chain of `bitXor(plus(x, c1), c2)` with distinct constants.
The guard is false on every step but reads the accumulator, so it cannot be folded away.

## Run

    # take the machine lock (kind: timing) first
    executor/bench/b2_block_dispatch/run.sh [--k 20000] [--links 100] [--repeats 3]
