import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
import fold
import config

# Write-log growth curve: every instruction is a store (worst case for the
# log). arrayPushBack always appends regardless of whether the target
# address repeats, so the write-log's array LENGTH grows to K (or the HWM)
# every run regardless of the decode table's own size -- DECN only needs to
# cover a small, fixed pool of addresses outside itself; it doesn't need to
# be >= K.
DECN = 8192           # small text/decode window, pc cycles through it repeatedly
RAM_WORDS = DECN + 1024  # text words 0..DECN-1, plus a small store target pool after it

def emit_schema():
    print("DROP TABLE IF EXISTS clickdoom_executor.ram;")
    print("""CREATE TABLE clickdoom_executor.ram (word_addr UInt32, value UInt32, version UInt64)
ENGINE = ReplacingMergeTree(version) ORDER BY word_addr;""")
    print(f"INSERT INTO clickdoom_executor.ram SELECT toUInt32(2147483648+number), 0, 0 FROM numbers({RAM_WORDS});")
    # Missing until #25 found it (same gap as executor/schema_fixture.sql's
    # test_fold.py fix): PR #88's MMIO plumbing added decode_with()'s KEYQT
    # subquery (reads {db}.input_queue) to every select_only() call,
    # including this one, but this bench's own schema never created the
    # table -- it would have failed outright (UNKNOWN_TABLE) the next time
    # anyone actually ran it, same as test_fold.py did.
    print("DROP TABLE IF EXISTS clickdoom_executor.input_queue;")
    print("""CREATE TABLE clickdoom_executor.input_queue (event_seq UInt64, key_event UInt16, consumed UInt8)
ENGINE = MergeTree ORDER BY event_seq;""")
    print("DROP TABLE IF EXISTS clickdoom_executor.decoded;")
    # Column names match sqlcpu/schema.sql (id/tgt/mk/sg), not SPEC §5's prose.
    print("""CREATE TABLE clickdoom_executor.decoded
(word_addr UInt32, id UInt8, rd UInt8, rs1 UInt8, rs2 UInt8, imm UInt32, tgt UInt32,
 mk UInt32, sg UInt8, raw UInt32) ENGINE = MergeTree ORDER BY word_addr;""")
    # sw x1, (RAM_BASE + DECN*4 + (number%256)*4)(x0) -- cycles through 256
    # addresses just past the text window (never self-modifies). tgt/sg are
    # irrelevant for a store (not a jump, not a load) so left at 0.
    print(f"""INSERT INTO clickdoom_executor.decoded
SELECT toUInt32(2147483648+number), {config.OP_STORE}, 0, 0, 1,
       toUInt32(2147483648 + {DECN*4} + (number%256)*4), 0, 4294967295, 0, 0
FROM numbers({DECN});""")

if __name__ == "__main__":
    if sys.argv[1] == "schema":
        emit_schema()
    else:
        K = int(sys.argv[1])
        print(fold.select_only(K, 0, DECN, DECN, RAM_WORDS, K + 1))  # hwm effectively disabled
