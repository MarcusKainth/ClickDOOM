//! This interpreter against the Python one, over generated cases.
//!
//! The committed traces and the recorded manifest cover the paths the ROM
//! takes. They say nothing about an illegal encoding, a misaligned target, a
//! store into the read-only region, or the corners of signed division,
//! because a working program never goes there. This does.
//!
//! Cases are structured rather than random words. A uniform 32-bit value is
//! almost always an illegal opcode and a uniform register value never lands
//! in a declared region, so uniform generation reaches the interesting paths
//! with probability near zero. Each case is a pure function of one seed, so a
//! failure replays from the seed alone.
//!
//! Run with `make fuzz-refemu-vs-python`. Exists for the migration and is
//! removed with the interpreter it compares against.
#![cfg(feature = "py-oracle")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use clickdoom_spec::{
    Checkpoint, FRAMEBUFFER_BASE, FRAMEBUFFER_SIZE, MMIO_BASE, MemoryMap, PALETTE_BASE,
    PALETTE_SIZE, RAM_BASE, TraceConfig,
};
use refemu::asm::*;
use refemu::trace::collect;
use refemu::{Cpu, Devices, Memory, decode};
use serde::{Deserialize, Serialize};

/// A machine small enough that a case is cheap and large enough to have
/// somewhere to store.
const RAM_SIZE: u32 = 64 * 1024;
/// Long enough for a loop to run and a fault to land after some real work.
const STEPS: u64 = 64;
/// Small enough that a case of `STEPS` instructions reaches both cadences.
const TRACE: TraceConfig = TraceConfig {
    checkpoint_interval: 4,
    ram_hash_interval: 16,
};
/// Cases per round trip to the oracle.
const BATCH: usize = 256;

// -- the case ---------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
struct Case {
    seed: u64,
    ram_size: u32,
    text: Option<(u32, u32)>,
    ipms: u32,
    pc: u32,
    regs: Vec<u32>,
    words: Vec<u32>,
    base: u32,
    keyq: Vec<(u8, u8)>,
    steps: u64,
    checkpoint_interval: u64,
    ram_hash_interval: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
struct HaltJson {
    reason: String,
    pc: u32,
    insn: Option<u32>,
    addr: Option<u32>,
    exit_code: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    icount: u64,
    #[serde(default)]
    pc: u32,
    #[serde(default)]
    regs: Vec<u32>,
    #[serde(default)]
    halt: Option<HaltJson>,
    #[serde(default)]
    reghash: u64,
    #[serde(default)]
    ramhash: u64,
    #[serde(default)]
    fbhash: u64,
    #[serde(default)]
    console: Vec<u8>,
    #[serde(default)]
    frame_commits: Vec<(u32, u64)>,
    #[serde(default)]
    keyq: Vec<(u8, u8)>,
    #[serde(default)]
    checkpoints: Vec<String>,
}

// -- generation -------------------------------------------------------------

/// A small deterministic generator, so a case is a pure function of a seed
/// and a failure replays without a corpus file.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let x = self.0;
        (x >> 33) ^ x
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len() as u64) as usize]
    }

    fn reg(&mut self) -> u32 {
        // x0 and the top register are over-represented, because those are
        // where a discarded write or a field-width mistake shows.
        let uniform = (self.next() % 32) as u32;
        self.pick(&[0u32, 1, 2, 3, 31, uniform])
    }
}

/// Values that sit on a boundary of the machine's arithmetic.
const SINGULAR: [u32; 12] = [
    0,
    1,
    2,
    7,
    0x7FFF_FFFF,
    0x8000_0000,
    0x8000_0001,
    0xFFFF_FFFF,
    0xFFFF_FFFE,
    0xFFFF_FFF9,
    0x4000_0000,
    0xC000_0000,
];

/// Addresses either side of every region's edge.
fn address_pool(ram_size: u32, text: Option<(u32, u32)>) -> Vec<u32> {
    let ram_end = RAM_BASE + ram_size;
    let mut pool = vec![
        0,
        4,
        0xFFFF_FFF0,
        RAM_BASE - 4,
        RAM_BASE,
        RAM_BASE + 1,
        RAM_BASE + 2,
        RAM_BASE + 3,
        ram_end - 4,
        ram_end - 2,
        ram_end,
        ram_end + 4,
        MMIO_BASE,
        MMIO_BASE + 4,
        MMIO_BASE + 8,
        MMIO_BASE + 0xC,
        MMIO_BASE + 0x10,
        MMIO_BASE + 0x14,
        MMIO_BASE + 0xFFC,
        MMIO_BASE + 0x1000,
        FRAMEBUFFER_BASE,
        FRAMEBUFFER_BASE + 1,
        FRAMEBUFFER_BASE + 2,
        FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE - 4,
        FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE,
        PALETTE_BASE,
        PALETTE_BASE + 1,
        PALETTE_BASE + PALETTE_SIZE - 4,
        PALETTE_BASE + PALETTE_SIZE,
    ];
    if let Some((start, end)) = text {
        pool.extend([start, start + 4, end - 4, end - 1, end, end + 4]);
    }
    pool
}

/// One legal instruction, drawn uniformly over the arms.
fn legal(rng: &mut Rng) -> u32 {
    let (rd, rs1, rs2) = (rng.reg(), rng.reg(), rng.reg());
    let imm = rng.pick(&[0i32, 1, -1, 4, -4, 2047, -2048, 16, -16]);
    let shamt = (rng.below(32)) as u32;
    let branch = rng.pick(&[0i32, 4, -4, 8, -8, 16, 2, -2]);
    match rng.below(41) {
        0 => lui(rd, imm),
        1 => auipc(rd, imm),
        2 => jal(rd, branch),
        3 => jalr(rd, rs1, imm),
        4 => beq(rs1, rs2, branch),
        5 => bne(rs1, rs2, branch),
        6 => blt(rs1, rs2, branch),
        7 => bge(rs1, rs2, branch),
        8 => bltu(rs1, rs2, branch),
        9 => bgeu(rs1, rs2, branch),
        10 => lb(rd, rs1, imm),
        11 => lh(rd, rs1, imm),
        12 => lw(rd, rs1, imm),
        13 => lbu(rd, rs1, imm),
        14 => lhu(rd, rs1, imm),
        15 => sb(rs1, rs2, imm),
        16 => sh(rs1, rs2, imm),
        17 => sw(rs1, rs2, imm),
        18 => addi(rd, rs1, imm),
        19 => slti(rd, rs1, imm),
        20 => sltiu(rd, rs1, imm),
        21 => xori(rd, rs1, imm),
        22 => ori(rd, rs1, imm),
        23 => andi(rd, rs1, imm),
        24 => slli(rd, rs1, shamt),
        25 => srli(rd, rs1, shamt),
        26 => srai(rd, rs1, shamt),
        27 => add(rd, rs1, rs2),
        28 => sub(rd, rs1, rs2),
        29 => sll(rd, rs1, rs2),
        30 => slt(rd, rs1, rs2),
        31 => sltu(rd, rs1, rs2),
        32 => xor(rd, rs1, rs2),
        33 => srl(rd, rs1, rs2),
        34 => sra(rd, rs1, rs2),
        35 => or(rd, rs1, rs2),
        36 => and(rd, rs1, rs2),
        37 => fence(),
        38 => ecall(),
        39 => ebreak(),
        _ => csrrw(rd, rs1, rng.pick(&[0x340i32, 0xC00, 0])),
    }
}

/// Sets a register to the device window and pokes every register in it.
///
/// Random code reaches these addresses too rarely to count on: a store needs
/// the right value in the right register and execution has to survive long
/// enough to run it. This makes the device paths reachable by construction,
/// so the corpus can say it covered them.
fn device_prologue(rng: &mut Rng) -> Vec<u32> {
    let mut words = vec![
        lui(5, 0x10000),
        addi(6, 0, rng.pick(&[0i32, 1, 65, 7, 0x141])),
        // The console, then a frame, then the two readable registers.
        sw(5, 6, 0x0C),
        sw(5, 6, 0x10),
        lw(7, 5, 0x04),
        lw(8, 5, 0x00),
    ];
    if rng.below(4) == 0 {
        // A clean stop, last, since nothing after it runs.
        words.push(sw(5, 6, 0x08));
    }
    words
}

/// One of the holes the decoder has, rather than a uniform word that would
/// almost always be an unassigned opcode.
fn structurally_illegal(rng: &mut Rng) -> u32 {
    let (rd, rs1, rs2) = (rng.reg(), rng.reg(), rng.reg());
    match rng.below(8) {
        0 => i_type(0x03, rd, rng.pick(&[3u32, 6, 7]), rs1, 0),
        1 => s_type(0x23, rng.pick(&[3u32, 4, 5, 6, 7]), rs1, rs2, 0),
        2 => b_type(0x63, rng.pick(&[2u32, 3]), rs1, rs2, 4),
        3 => i_type(0x67, rd, rng.below(7) as u32 + 1, rs1, 0),
        4 => r_type(
            0x33,
            rd,
            rng.below(8) as u32,
            rs1,
            rs2,
            rng.pick(&[2u32, 3, 0x7F]),
        ),
        5 => r_type(
            0x13,
            rd,
            rng.pick(&[1u32, 5]),
            rs1,
            rs2,
            rng.pick(&[0x10u32, 0x40]),
        ),
        6 => i_type(
            0x73,
            rd,
            rng.pick(&[0u32, 4]),
            rs1,
            rng.pick(&[2i32, 3, 0x340]),
        ),
        _ => (rng.next() as u32) & !0b11 | rng.pick(&[0u32, 1, 2]),
    }
}

fn m_extension(rng: &mut Rng) -> u32 {
    let (rd, rs1, rs2) = (rng.reg(), rng.reg(), rng.reg());
    r_type(0x33, rd, rng.below(8) as u32, rs1, rs2, 0x01)
}

/// A load or store whose address register the case seeds from the pool.
fn memory_op(rng: &mut Rng) -> u32 {
    let (rd, rs1, rs2) = (rng.reg(), rng.reg(), rng.reg());
    let imm = rng.pick(&[0i32, 1, 2, 3, -1, -4, 4, 2047, -2048]);
    match rng.below(8) {
        0 => lb(rd, rs1, imm),
        1 => lh(rd, rs1, imm),
        2 => lw(rd, rs1, imm),
        3 => lbu(rd, rs1, imm),
        4 => lhu(rd, rs1, imm),
        5 => sb(rs1, rs2, imm),
        6 => sh(rs1, rs2, imm),
        _ => sw(rs1, rs2, imm),
    }
}

/// A jump or branch computing a target that is not four-byte aligned.
fn bad_target(rng: &mut Rng) -> u32 {
    let (rd, rs1, rs2) = (rng.reg(), rng.reg(), rng.reg());
    let odd = rng.pick(&[2i32, 6, -2, -6]);
    match rng.below(3) {
        0 => jal(rd, odd),
        1 => jalr(rd, rs1, odd),
        _ => beq(rs1, rs2, odd),
    }
}

fn make_case(seed: u64) -> Case {
    let mut rng = Rng(seed ^ 0x9E37_79B9_7F4A_7C15);
    // A region in half the corpus, so the self-modify path is live for half
    // of it and absent for the other half.
    let text = (rng.below(2) == 0).then_some((RAM_BASE, RAM_BASE + 0x200));
    let pool = address_pool(RAM_SIZE, text);

    let mut words = Vec::with_capacity(64);
    // A fifth of the corpus starts by talking to the devices.
    if rng.below(5) == 0 {
        words.extend(device_prologue(&mut rng));
    }
    let word_count = 8 + rng.below(56) as usize;
    for _ in 0..word_count {
        words.push(match rng.below(100) {
            0..60 => legal(&mut rng),
            60..75 => structurally_illegal(&mut rng),
            75..85 => m_extension(&mut rng),
            85..95 => memory_op(&mut rng),
            _ => bad_target(&mut rng),
        });
    }

    let mut regs = vec![0u32; 32];
    for slot in regs.iter_mut().skip(1) {
        *slot = match rng.below(3) {
            0 => rng.pick(&SINGULAR),
            1 => rng.pick(&pool),
            _ => rng.next() as u32,
        };
    }

    let keyq: Vec<(u8, u8)> = (0..rng.below(4) + 1)
        .map(|_| ((rng.below(2)) as u8, rng.next() as u8))
        .collect();

    Case {
        seed,
        ram_size: RAM_SIZE,
        text,
        // At the shipped value a short case never moves the tick register,
        // which is what "nominally covered" looks like.
        ipms: rng.pick(&[1u32, 2, 10, 10_000]),
        pc: RAM_BASE + 4 * rng.below(4) as u32,
        regs,
        words,
        base: RAM_BASE,
        keyq,
        steps: STEPS,
        checkpoint_interval: TRACE.checkpoint_interval,
        ram_hash_interval: TRACE.ram_hash_interval,
    }
}

// -- running this side ------------------------------------------------------

fn run_here(case: &Case) -> Answer {
    let map = MemoryMap::clickdoom().with_ram_size(case.ram_size);
    let memory = Memory::new(map, Devices::registers(case.ipms));
    let mut cpu = Cpu::new(memory, case.pc);
    cpu.load_image(&program(&case.words), case.base).unwrap();
    cpu.set_text_region(case.text);
    cpu.enable_decode_cache();
    cpu.set_pc(case.pc);
    for (index, value) in case.regs.iter().enumerate() {
        cpu.set_register(index as u8, *value);
    }
    {
        let registers = cpu.memory.devices_mut().registers_mut().unwrap();
        for (pressed, doomkey) in &case.keyq {
            registers.push_key(*pressed != 0, *doomkey);
        }
    }

    let config = TraceConfig {
        checkpoint_interval: case.checkpoint_interval,
        ram_hash_interval: case.ram_hash_interval,
    };
    let (lines, stop) = collect(&mut cpu, config, case.steps);
    let registers = cpu.memory.devices().registers_ref().unwrap();
    Answer {
        error: None,
        icount: cpu.icount(),
        pc: cpu.pc(),
        regs: cpu.regs().to_vec(),
        halt: stop.halt().map(|h| HaltJson {
            reason: h.reason.to_string(),
            pc: h.pc,
            insn: h.insn,
            addr: h.addr,
            exit_code: h.exit_code,
        }),
        reghash: refemu::trace::reg_hash_of(&cpu),
        ramhash: refemu::trace::ram_hash_of(&cpu),
        fbhash: refemu::trace::fb_hash_of(&cpu),
        console: registers.console.clone(),
        frame_commits: registers
            .frame_commits
            .iter()
            .map(|c| (c.frame_no, c.commit_icount))
            .collect(),
        keyq: registers
            .key_queue
            .iter()
            .map(|k| (k.pressed as u8, k.doomkey))
            .collect(),
        checkpoints: lines.iter().map(Checkpoint::to_string).collect(),
    }
}

// -- comparing --------------------------------------------------------------

/// Names every field that differs, rather than one hash over everything: a
/// single mismatched digest says nothing about where to look.
fn differences(ours: &Answer, theirs: &Answer) -> Vec<String> {
    let mut out = Vec::new();
    let mut note = |name: &str, a: String, b: String| {
        if a != b {
            out.push(format!("{name}: rust={a} python={b}"));
        }
    };
    note("icount", ours.icount.to_string(), theirs.icount.to_string());
    note(
        "pc",
        format!("{:#010x}", ours.pc),
        format!("{:#010x}", theirs.pc),
    );
    for index in 0..32 {
        note(
            &format!("x{index}"),
            format!("{:#010x}", ours.regs.get(index).copied().unwrap_or(0)),
            format!("{:#010x}", theirs.regs.get(index).copied().unwrap_or(0)),
        );
    }
    match (&ours.halt, &theirs.halt) {
        (Some(a), Some(b)) => {
            note("halt.reason", a.reason.clone(), b.reason.clone());
            note(
                "halt.pc",
                format!("{:#010x}", a.pc),
                format!("{:#010x}", b.pc),
            );
            note(
                "halt.insn",
                format!("{:?}", a.insn),
                format!("{:?}", b.insn),
            );
            note(
                "halt.addr",
                format!("{:?}", a.addr),
                format!("{:?}", b.addr),
            );
            note(
                "halt.exit_code",
                format!("{:?}", a.exit_code),
                format!("{:?}", b.exit_code),
            );
        }
        (a, b) => note("halt", format!("{a:?}"), format!("{b:?}")),
    }
    note(
        "reghash",
        format!("{:016x}", ours.reghash),
        format!("{:016x}", theirs.reghash),
    );
    note(
        "ramhash",
        format!("{:016x}", ours.ramhash),
        format!("{:016x}", theirs.ramhash),
    );
    note(
        "fbhash",
        format!("{:016x}", ours.fbhash),
        format!("{:016x}", theirs.fbhash),
    );
    note(
        "console",
        format!("{:?}", ours.console),
        format!("{:?}", theirs.console),
    );
    note(
        "frame_commits",
        format!("{:?}", ours.frame_commits),
        format!("{:?}", theirs.frame_commits),
    );
    note(
        "keyq",
        format!("{:?}", ours.keyq),
        format!("{:?}", theirs.keyq),
    );
    note(
        "checkpoints",
        format!("{:?}", ours.checkpoints),
        format!("{:?}", theirs.checkpoints),
    );
    out
}

// -- what the corpus actually reached ---------------------------------------

/// Counted from what the oracle answered, not from what the generator meant
/// to produce. An arm that intended a self-modify and got a bad address
/// increments the bad address.
#[derive(Default)]
struct Hits {
    outcomes: BTreeMap<String, u64>,
    mnemonics: BTreeMap<String, u64>,
}

impl Hits {
    fn record(&mut self, case: &Case, answer: &Answer) {
        let bump = |map: &mut BTreeMap<String, u64>, key: &str| {
            *map.entry(key.to_owned()).or_insert(0) += 1;
        };
        for word in &case.words {
            bump(&mut self.mnemonics, decode(*word).op.mnemonic());
        }
        match &answer.halt {
            None => bump(&mut self.outcomes, "no-halt"),
            Some(halt) => {
                bump(&mut self.outcomes, &halt.reason);
                let addr = halt.addr.unwrap_or(0);
                match halt.reason.as_str() {
                    "BAD_ADDR" => {
                        if addr >= RAM_BASE + case.ram_size && addr < RAM_BASE + case.ram_size + 64
                        {
                            bump(&mut self.outcomes, "bad_addr_just_past_ram");
                        }
                        if (FRAMEBUFFER_BASE..FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE).contains(&addr) {
                            bump(&mut self.outcomes, "fb_subword_store");
                        }
                        if (PALETTE_BASE..PALETTE_BASE + PALETTE_SIZE).contains(&addr) {
                            bump(&mut self.outcomes, "pal_subword_store");
                        }
                    }
                    "MISALIGNED" => {
                        bump(
                            &mut self.outcomes,
                            if addr % 4 == 2 {
                                "misaligned_by_two"
                            } else {
                                "misaligned_by_one_or_three"
                            },
                        );
                    }
                    "SELF_MODIFY" => {
                        if let Some((start, end)) = case.text
                            && (start..end).contains(&addr)
                        {
                            bump(&mut self.outcomes, "self_modify_inside_text");
                        }
                    }
                    _ => {}
                }
            }
        }
        if !answer.console.is_empty() {
            bump(&mut self.outcomes, "putchar");
        }
        if !answer.frame_commits.is_empty() {
            bump(&mut self.outcomes, "frame_commit");
        }
        if answer.keyq.len() < case.keyq.len() {
            bump(&mut self.outcomes, "keyq_pop_nonempty");
        }
        if !answer.checkpoints.is_empty() {
            bump(&mut self.outcomes, "checkpoint_line_emitted");
        }
        if answer
            .checkpoints
            .iter()
            .any(|line| line.matches('\t').count() == 4)
        {
            bump(&mut self.outcomes, "ram_hash_line_emitted");
        }
    }

    fn report(&self) -> String {
        let mut out = String::from("outcomes observed:\n");
        for (name, count) in &self.outcomes {
            out.push_str(&format!("  {name:28} {count}\n"));
        }
        out.push_str("mnemonics generated:\n");
        for (name, count) in &self.mnemonics {
            out.push_str(&format!("  {name:28} {count}\n"));
        }
        out
    }

    /// Every path the corpus is meant to reach, so a run that reached none of
    /// them fails rather than reporting the same green result as one that
    /// reached all of them.
    fn missing(&self) -> Vec<String> {
        const OUTCOMES: [&str; 16] = [
            "no-halt",
            "ILLEGAL_INSN",
            "BAD_ADDR",
            "SELF_MODIFY",
            "MISALIGNED",
            "ECALL",
            "EBREAK",
            "CSR",
            "EXIT",
            "bad_addr_just_past_ram",
            "self_modify_inside_text",
            "misaligned_by_two",
            "putchar",
            "frame_commit",
            "checkpoint_line_emitted",
            "ram_hash_line_emitted",
        ];
        let mut missing: Vec<String> = OUTCOMES
            .iter()
            .filter(|name| !self.outcomes.contains_key(**name))
            .map(|name| format!("outcome {name}"))
            .collect();
        for op in ALL_MNEMONICS {
            if !self.mnemonics.contains_key(op) {
                missing.push(format!("mnemonic {op}"));
            }
        }
        missing
    }
}

const ALL_MNEMONICS: [&str; 49] = [
    "lui", "auipc", "jal", "jalr", "beq", "bne", "blt", "bge", "bltu", "bgeu", "lb", "lh", "lw",
    "lbu", "lhu", "sb", "sh", "sw", "addi", "slti", "sltiu", "xori", "ori", "andi", "slli", "srli",
    "srai", "add", "sub", "sll", "slt", "sltu", "xor", "srl", "sra", "or", "and", "mul", "mulh",
    "mulhsu", "mulhu", "div", "divu", "rem", "remu", "fence", "ecall", "ebreak", "csr",
];

// -- the oracle -------------------------------------------------------------

struct Oracle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Oracle {
    fn start() -> Self {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("py_oracle")
            .join("oracle.py");
        // The oracle needs the interpreter's own environment, which is
        // where its hash library lives. REFEMU_PYTHON overrides.
        let python = std::env::var("REFEMU_PYTHON").unwrap_or_else(|_| {
            let venv = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(".venv")
                .join("bin")
                .join("python");
            if venv.exists() {
                venv.to_string_lossy().into_owned()
            } else {
                "python3".to_owned()
            }
        });
        let mut child = Command::new(python)
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("starting {}: {e}", script.display()));
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn ask(&mut self, batch: &[Case]) -> Vec<Answer> {
        let line = serde_json::to_string(batch).unwrap();
        writeln!(self.stdin, "{line}").expect("the oracle stopped reading");
        self.stdin.flush().unwrap();
        let mut reply = String::new();
        self.stdout
            .read_line(&mut reply)
            .expect("the oracle stopped answering");
        assert!(!reply.is_empty(), "the oracle answered nothing");
        serde_json::from_str(&reply).expect("the oracle's answer is not readable")
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Makes a failing case smaller while it keeps failing the same way.
///
/// Deterministic: bisect the step count, drop instructions from each end,
/// zero the registers, replace instructions with a no-op, halve the
/// immediates. A change is kept only when the first differing field stays the
/// same, so shrinking cannot wander onto a different bug.
fn shrink(oracle: &mut Oracle, case: &Case, first_field: &str) -> Case {
    let fails_same = |oracle: &mut Oracle, candidate: &Case| -> bool {
        let theirs = oracle.ask(std::slice::from_ref(candidate));
        if theirs[0].error.is_some() {
            return false;
        }
        differences(&run_here(candidate), &theirs[0])
            .first()
            .is_some_and(|d| d.split(':').next() == Some(first_field))
    };

    let mut best = case.clone();
    loop {
        let before = (best.words.len(), best.steps, best.regs.clone());

        let mut steps = best.steps;
        while steps > 1 {
            let mut candidate = best.clone();
            candidate.steps = steps / 2;
            if fails_same(oracle, &candidate) {
                best = candidate;
                steps /= 2;
            } else {
                break;
            }
        }
        while best.words.len() > 1 {
            let mut candidate = best.clone();
            candidate.words.pop();
            if fails_same(oracle, &candidate) {
                best = candidate;
            } else {
                break;
            }
        }
        for index in (1..32).rev() {
            if best.regs[index] == 0 {
                continue;
            }
            let mut candidate = best.clone();
            candidate.regs[index] = 0;
            if fails_same(oracle, &candidate) {
                best = candidate;
            }
        }
        for index in 0..best.words.len() {
            if best.words[index] == NOP {
                continue;
            }
            let mut candidate = best.clone();
            candidate.words[index] = NOP;
            if fails_same(oracle, &candidate) {
                best = candidate;
            }
        }
        if (best.words.len(), best.steps, best.regs.clone()) == before {
            return best;
        }
    }
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("fuzz-failures")
}

fn cases() -> u64 {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

fn first_seed() -> u64 {
    std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

#[test]
fn the_two_interpreters_agree() {
    let total = cases();
    let start = first_seed();
    let mut oracle = Oracle::start();
    let mut hits = Hits::default();
    let mut failures: Vec<String> = Vec::new();

    let mut seed = start;
    while seed < start + total {
        let batch: Vec<Case> = (seed..(seed + BATCH as u64).min(start + total))
            .map(make_case)
            .collect();
        seed += batch.len() as u64;
        let theirs = oracle.ask(&batch);
        assert_eq!(
            theirs.len(),
            batch.len(),
            "the oracle answered a short batch"
        );

        for (case, answer) in batch.iter().zip(&theirs) {
            if let Some(error) = &answer.error {
                failures.push(format!("seed {}: the oracle failed: {error}", case.seed));
                continue;
            }
            hits.record(case, answer);
            let ours = run_here(case);
            let differing = differences(&ours, answer);
            if differing.is_empty() {
                continue;
            }
            let field = differing[0].split(':').next().unwrap_or("").to_owned();
            let smaller = shrink(&mut oracle, case, &field);
            std::fs::create_dir_all(corpus_dir()).ok();
            let path = corpus_dir().join(format!("{}.json", case.seed));
            std::fs::write(&path, serde_json::to_string_pretty(&smaller).unwrap()).ok();
            failures.push(format!(
                "seed {}:\n  {}\n  shrunk to {} words, {} steps, saved at {}",
                case.seed,
                differing.join("\n  "),
                smaller.words.len(),
                smaller.steps,
                path.display()
            ));
            if failures.len() >= 5 {
                break;
            }
        }
        if failures.len() >= 5 {
            break;
        }
    }

    println!("{total} cases from seed {start}\n{}", hits.report());
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));

    // A run that compared nothing reports the same green result as one that
    // compared everything, unless it has to say what it reached.
    let missing = hits.missing();
    assert!(
        missing.is_empty(),
        "the corpus never reached: {}",
        missing.join(", ")
    );
}

/// Proves the differential can fail.
///
/// Only meaningful with the mutation compiled in, so with it off this asserts
/// the opposite: that the unmutated build agrees. Either way the test says
/// which build it ran against.
#[test]
fn the_differential_catches_a_broken_semantic() {
    let mut oracle = Oracle::start();
    // An unsigned division by zero, which the mutation gets wrong.
    let case = Case {
        seed: 0,
        ram_size: RAM_SIZE,
        text: None,
        ipms: 10_000,
        pc: RAM_BASE,
        regs: {
            let mut regs = vec![0u32; 32];
            regs[1] = 5;
            regs[2] = 0;
            regs
        },
        words: vec![divu(3, 1, 2), ecall()],
        base: RAM_BASE,
        keyq: Vec::new(),
        steps: 8,
        checkpoint_interval: TRACE.checkpoint_interval,
        ram_hash_interval: TRACE.ram_hash_interval,
    };
    let theirs = oracle.ask(std::slice::from_ref(&case));
    assert_eq!(theirs[0].error, None);
    let differing = differences(&run_here(&case), &theirs[0]);

    if cfg!(feature = "fuzz-selftest") {
        assert!(
            differing.iter().any(|d| d.starts_with("x3:")),
            "the mutated build was not caught: {differing:?}"
        );
        println!("the mutation was caught: {}", differing.join(", "));
    } else {
        assert!(
            differing.is_empty(),
            "the unmutated build disagrees: {differing:?}"
        );
        println!("no mutation compiled in, and the two agree on x3");
    }
}
