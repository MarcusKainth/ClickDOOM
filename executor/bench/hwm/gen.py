import sys
import os; sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
import fold, config

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
    print("DROP TABLE IF EXISTS clickdoom_executor.decoded;")
    print("""CREATE TABLE clickdoom_executor.decoded
(word_addr UInt32, op_id UInt8, rd UInt8, rs1 UInt8, rs2 UInt8, imm UInt32, target UInt32,
 width_mask UInt32, sign_bit UInt32, raw UInt32) ENGINE = MergeTree ORDER BY word_addr;""")
    # sw x1, (RAM_BASE + DECN*4 + (number%256)*4)(x0) -- cycles through 256
    # addresses just past the text window (never self-modifies).
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
