# A1 — why ClickHouse's expression JIT only partially compiles the `arrayFold` step

## The question

`compile_expressions = 1` gives a small synthetic fold-lambda **5.6x**
(`executor/bench/e1_cse/README.md`, the JIT sub-experiment) but gives
`executor/fold.py`'s real step expression only **~1.23x**. The prior evidence
for "only part of the real expression qualifies for compilation" was a
production reading of

    ProfileEvents['CompileFunction']                = 3
    ProfileEvents['CompileExpressionsMicroseconds']  = 12652

on the 4th of six chained batches.

> Which constructs in the step expression block JIT compilation; does one
> non-compilable node poison the whole enclosing subtree or only itself; and
> how much of the ~104,000-node expression is actually handed to LLVM?

## How to rerun

    # a container whose compiled-expression cache is cold, so CompileFunction
    # is readable at all (see trap 1). Never measure the JIT on the shared
    # clickdoom-ch: other agents have already warmed it.
    docker run -d --name a1-jit-ch --ulimit nofile=262144 \
      -e CLICKHOUSE_PASSWORD=clickdoom clickhouse/clickhouse-server:26.3
    docker exec -i a1-jit-ch clickhouse-client --multiquery < setup.sql

    cd executor/bench/a1_jit
    CLICKDOOM_CH_CONTAINER=a1-jit-ch A1_SETUP=0 ./run.sh        # pass 1: island counts
    CLICKDOOM_CH_CONTAINER=a1-jit-ch ./run_series.sh            # pass 2: timing series
    CLICKDOOM_CH_CONTAINER=a1-jit-ch ./real_fold.sh             # pass 3: the REAL fold

    # pass 2 variations
    A1_JIT=0 ./run_series.sh                    # interpreted baseline
    A1_MINCOUNT=3 A1_REPEATS=6 ./run_series.sh  # the 4th-run step change

    docker rm -f a1-jit-ch                      # clean up
    docker exec -i clickdoom-ch clickhouse-client --query 'DROP DATABASE a1_jit_bench'

`real_fold.sh` runs `executor/fold.py` **unmodified**, pointed at this
experiment's own database via the `--db` override fold.py already provides.
Nothing in this directory writes to production code or production tables.
