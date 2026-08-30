# Write-log high-water mark

Sweeps K and measures where the write-log flush stops being cheap, which is what
sets `WRITE_LOG_HIGH_WATER_MARK_DEFAULT` in `executor/config.py`.

Findings are in [RESULTS.md](RESULTS.md).

## Run

    make bench-hwm

Override the sweep with `CLICKDOOM_HWM_KS`.

## What it needs

A quiet machine, and the shared container from `make up`.
