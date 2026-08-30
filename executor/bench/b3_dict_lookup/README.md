# B3: dictGet against arrayElement for RAM reads inside arrayFold

Would a FLAT or HASHED dictionary beat the captured constant array the fold reads RAM from?
Measured on the repo pin, 26.7.5.10, fresh container, `max_threads=1`, `compile_expressions=0`, K=20,000 steps, a dense 6,291,456-word table.

## Run

    # take the machine lock (kind: timing) first
    executor/bench/b3_dict_lookup/run.sh [--k 20000] [--repeats 3]
