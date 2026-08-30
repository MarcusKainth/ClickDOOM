#!/usr/bin/env python3
"""Captures fold.py's and commit.py's current SQL output into
executor/tests/golden/*.sql, minus embedded `-- ` comment lines. The Rust
port's tests assert byte-for-byte equality against these fixtures for the
same inputs: a bare string compare, nothing else normalized, so a whitespace
or formatting drift fails as loudly as a wrong keyword. Comments are the one
deliberate difference: ClickHouse strips them before parsing, so they never
reach the AST or the compiled-expression cache key, and the Rust port drops
them rather than carrying the Python source's own comment conventions into
generated SQL text.

Regenerate only while the Python originals still exist. After they are
deleted, these fixtures are the frozen record; a further intentional
SQL-shape change is a diff to a checked-in .sql file plus its fixture, not a
diff to this generator.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import commit
import config
import fold

HERE = Path(__file__).resolve().parent


def strip_sql_comments(text: str) -> str:
    return "\n".join(
        line for line in text.split("\n") if not line.strip().startswith("--")
    )


def write(name: str, text: str) -> None:
    text = strip_sql_comments(text)
    path = HERE / f"{name}.sql"
    path.write_bytes(text.encode("utf-8"))
    print(f"{name}: {len(text)} chars")


def main() -> None:
    # Production sizing: TEXT_WORDS_DEFAULT/RAM_WORDS_DEFAULT, the shape
    # _fb_pal_wa_provably_outside_text proves True for (build_step()'s SQL
    # drops the runtime is_fb_or_pal_store guard on the SELF_MODIFY arm).
    write(
        "select_only_prod_k1",
        fold.select_only(
            1, 0, config.TEXT_WORDS_DEFAULT, config.TEXT_WORDS_DEFAULT,
            config.RAM_WORDS_DEFAULT, config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT,
        ),
    )

    # Small-fixture sizing matching test_framebuffer_store_does_not_trigger_self_modify:
    # ram_words == decn == text_end_widx == 8, the shape the proof returns
    # False for (build_step()'s SQL keeps the runtime guard).
    write(
        "select_only_small_k2",
        fold.select_only(2, 0, 8, 8, 8, 10_000),
    )

    # Every optional argument overridden, production sizing: exercises the
    # regs0 list-literal branch, a non-default wl0/keyq0/icount0/hwm/ipms/db.
    write(
        "select_only_overrides",
        fold.select_only(
            4096, 0, config.TEXT_WORDS_DEFAULT, config.TEXT_WORDS_DEFAULT,
            config.RAM_WORDS_DEFAULT, 15_000,
            pc0=0x80001000, regs0=list(range(1, 32)),
            db="other_db", icount0=12345, keyq0=7, ipms=5_000,
            wl0="tuple([1,2,3], [10,20,30], [100,200,300])",
        ),
    )

    # batch(): one production-shaped case, the K canonical_throughput uses.
    write(
        "batch_prod",
        fold.batch(
            60_000, 0, config.TEXT_WORDS_DEFAULT, config.TEXT_WORDS_DEFAULT,
            config.RAM_WORDS_DEFAULT, config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT,
        ),
    )

    write("halt_reason_transform", fold._halt_reason_transform("r.4.3"))

    # commit.py: default db, one non-default db, one non-default retention n.
    write("ram_flush_default", commit.ram_flush_sql())
    write("ram_flush_other_db", commit.ram_flush_sql(db="other_db"))
    write("fbpal_flush_default", commit.fbpal_flush_sql())
    write("console_out_flush_default", commit.console_out_flush_sql())
    write("cpu_state_flush_default", commit.cpu_state_flush_sql())
    write("retention_default", commit.retention_sql())
    write("retention_n10", commit.retention_sql(n=10))


if __name__ == "__main__":
    main()
