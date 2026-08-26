"""A tiny, independent Python interpreter over the SAME collapsed op_id
representation fold.py implements (ADR-0002's arms plus #23's 28-31).

This is NOT a RV32IM interpreter and is not a substitute for refemu -- it
exists to answer one narrower question with an oracle independent of
fold.py's own SQL: given a stream of already-decoded (op_id, rd, rs1, rs2,
imm, target, width_mask, sign_bit) rows, does the fold correctly implement
the *collapsed* semantics #23's design claims? Decode correctness (does a
real `addi` produce the right op_id/imm) is sqlcpu's #18/#19, checked later
via SPEC §7 differential runs once that lands.
"""
import dataclasses

U32 = 0xFFFFFFFF


def u32(x):
    return x & U32


def s32(x):
    x = u32(x)
    return x - (1 << 32) if x & 0x8000_0000 else x


@dataclasses.dataclass
class Insn:
    op_id: int
    rd: int = 0
    rs1: int = 0
    rs2: int = 0
    imm: int = 0
    target: int = 0
    width_mask: int = 0xFFFFFFFF
    sign_bit: int = 0
    raw: int = 0


HALT_NONE, HALT_ILLEGAL_INSN, HALT_SELF_MODIFY, HALT_BAD_ADDR, HALT_MISALIGNED, \
    HALT_ECALL, HALT_EBREAK, HALT_CSR = range(8)

OP_LOAD, OP_STORE = 18, 19
OP_ECALL, OP_EBREAK, OP_CSR, OP_ILLEGAL = 28, 29, 30, 31


def run(insns, ram_base, ram_words, text_start_widx, text_end_widx,
        regs0=None, pc0=0, k=None, hwm=10_000, ram0=None):
    """insns: list[Insn], index 0..len-1 is the decode table (word index
    relative to the text window, matching fold.py's IDX addressing).
    ram0: dict word_index -> value (relative to ram_base, word granularity)
    -- the memory this run starts from; NOT applied unless passed (a prior
    version of this function silently ignored the caller's initial RAM
    entirely, always starting from an empty memory).
    Returns a dict mirroring the fold's accumulator fields."""
    regs = list(regs0) if regs0 else [0] * 32
    ram = dict(ram0) if ram0 else {}
    wl_addr, wl_val, wl_icount = [], [], []
    pc = pc0
    stopped = halted = False
    halt_reason = HALT_NONE
    halt_pc = halt_extra = 0
    retired = 0
    k = k if k is not None else len(insns)

    def mem_read_word(widx):
        for a, v in zip(reversed(wl_addr), reversed(wl_val)):
            if a == widx:
                return v
        return ram.get(widx, 0)

    for _ in range(k):
        if stopped:
            break
        ins = insns[pc] if 0 <= pc < len(insns) else Insn(op_id=OP_ILLEGAL, raw=0)
        a = regs[ins.rs1]
        b = u32(regs[ins.rs2] + ins.imm)
        sa, sb = s32(a), s32(b)
        addr = u32(a + ins.imm)
        is_mem = ins.op_id in (OP_LOAD, OP_STORE)
        bad_addr = is_mem and not (ram_base <= addr < ram_base + ram_words * 4)
        align_mask = 3 if ins.width_mask == 0xFFFFFFFF else (1 if ins.width_mask == 0xFFFF else 0)
        misaligned = is_mem and not bad_addr and (addr & align_mask) != 0
        wa = ((addr - ram_base) >> 2) & (ram_words - 1)
        self_modify = (ins.op_id == OP_STORE and not bad_addr and not misaligned
                       and text_start_widx <= wa < text_end_widx)
        decode_fatal = ins.op_id in (OP_ECALL, OP_EBREAK, OP_CSR, OP_ILLEGAL)
        halts_now = decode_fatal or bad_addr or misaligned or self_modify

        if halts_now:
            halted = True
            stopped = True
            halt_pc = pc
            if ins.op_id == OP_ILLEGAL:
                halt_reason, halt_extra = HALT_ILLEGAL_INSN, ins.raw
            elif self_modify:
                halt_reason, halt_extra = HALT_SELF_MODIFY, addr
            elif bad_addr:
                halt_reason, halt_extra = HALT_BAD_ADDR, addr
            elif misaligned:
                halt_reason, halt_extra = HALT_MISALIGNED, addr
            elif ins.op_id == OP_ECALL:
                halt_reason = HALT_ECALL
            elif ins.op_id == OP_EBREAK:
                halt_reason = HALT_EBREAK
            elif ins.op_id == OP_CSR:
                halt_reason = HALT_CSR
            break

        sh = 8 * (addr & 3)
        if ins.op_id == OP_LOAD:
            lw = mem_read_word(wa)
            v = (lw >> sh) & ins.width_mask
            if v & ins.sign_bit:
                v = u32(v - (ins.width_mask + 1))
            result = v
        elif ins.op_id == OP_STORE:
            lw = mem_read_word(wa)
            result = None
        else:
            result = _alu(ins.op_id, a, b, sa, sb, ins.target)

        nxt = _next_pc(ins, a, b, sa, sb, pc, text_end_widx)

        if ins.op_id == OP_STORE:
            # value is raw regs[rs2], not b (= regs[rs2] + imm) -- imm is the
            # address offset, already spent on addr/wa above.
            sval = u32((lw & u32(~(ins.width_mask << sh))) | ((regs[ins.rs2] & ins.width_mask) << sh))
            wl_addr.append(wa)
            wl_val.append(sval)
            wl_icount.append(retired + 1)
        elif ins.rd != 0:
            regs[ins.rd] = u32(result)

        pc = nxt
        retired += 1
        if ins.op_id == OP_STORE and len(wl_addr) >= hwm:
            stopped = True

    return dict(pcidx=pc, regs=regs, wl_addr=wl_addr, wl_val=wl_val, wl_icount=wl_icount,
                stopped=int(stopped), halted=int(halted), halt_reason=halt_reason,
                halt_pc=halt_pc, halt_extra=halt_extra, retired=retired)


def _alu(op_id, a, b, sa, sb, target):
    if op_id == 0: return u32(a + b)
    if op_id == 1: return u32(a - b)
    if op_id == 2: return u32(a << (b & 31))
    if op_id == 3: return int(sa < sb)
    if op_id == 4: return int(a < b)
    if op_id == 5: return a ^ b
    if op_id == 6: return a >> (b & 31)
    if op_id == 7: return u32(sa >> (b & 31))  # Python's >> on a negative int is arithmetic
    if op_id == 8: return a | b
    if op_id == 9: return a & b
    if op_id == 10: return u32(sa * sb)
    if op_id == 11: return u32((sa * sb) >> 32)
    if op_id == 12: return u32((sa * b) >> 32)  # mulhsu: signed(a) * unsigned(b)
    if op_id == 13: return u32((a * b) >> 32)
    if op_id == 14: return U32 if sb == 0 else u32(_intdiv(sa, sb))
    if op_id == 15: return U32 if b == 0 else a // b
    if op_id == 16: return a if sb == 0 else u32(_mod(sa, sb))
    if op_id == 17: return a if b == 0 else a % b
    return target


def _intdiv(a, b):
    q = abs(a) // abs(b)
    return -q if (a < 0) != (b < 0) else q


def _mod(a, b):
    r = abs(a) % abs(b)
    return -r if a < 0 else r


def _next_pc(ins, a, b, sa, sb, pc, decn):
    decm = decn - 1
    if ins.op_id == 20: return ins.target if a == b else u32(pc + 1) & decm
    if ins.op_id == 21: return ins.target if a != b else u32(pc + 1) & decm
    if ins.op_id == 22: return ins.target if sa < sb else u32(pc + 1) & decm
    if ins.op_id == 23: return ins.target if sa >= sb else u32(pc + 1) & decm
    if ins.op_id == 24: return ins.target if a < b else u32(pc + 1) & decm
    if ins.op_id == 25: return ins.target if a >= b else u32(pc + 1) & decm
    if ins.op_id == 26: return ins.target
    if ins.op_id == 27: return (u32(a + ins.imm) >> 2) & decm
    return (pc + 1) & decm
