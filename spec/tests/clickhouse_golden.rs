//! The checkpoint hashes against ClickHouse's own `xxHash64`.
//!
//! These four values are the load-bearing ones. They are what says the seed is
//! 0, that the register buffer is `pc` then `x1` through `x31` little-endian,
//! that the RAM hash covers plain address-ascending bytes, and that the
//! framebuffer hash concatenates in that order.
//!
//! This is the only place they are pinned. `driver/tests/sqlcpu_live.rs`
//! checks the SQL against the functions below rather than against a second
//! copy of the numbers.
//!
//! Running with `--nocapture` prints the query that re-derives each one, so
//! the check can be reproduced against the pinned server rather than trusted.

use clickdoom_spec::{fb_hash, ram_hash, reg_hash};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn reg_hash_buffer(pc: u32, regs: &[u32; 32]) -> Vec<u8> {
    let mut buf = pc.to_le_bytes().to_vec();
    for reg in &regs[1..32] {
        buf.extend_from_slice(&reg.to_le_bytes());
    }
    buf
}

fn report(name: &str, bytes: &[u8], expected: u64) {
    println!("-- {name}: expect {expected}");
    println!("SELECT xxHash64(unhex('{}'));", hex(bytes));
}

#[test]
fn reg_hash_with_every_hashed_register_zero() {
    let regs = [0u32; 32];
    let pc = 0x8000_0004;
    let buffer = reg_hash_buffer(pc, &regs);
    assert_eq!(buffer.len(), 128);
    report("reg_hash, pc only", &buffer, 4_903_144_380_889_844_081);
    assert_eq!(reg_hash(pc, &regs), 4_903_144_380_889_844_081);
}

#[test]
fn reg_hash_with_registers_set() {
    let mut regs = [0u32; 32];
    regs[1] = 0xDEAD_BEEF;
    regs[10] = 42;
    regs[31] = 0xFFFF_FFFF;
    let pc = 0x8000_0100;
    report(
        "reg_hash, registers set",
        &reg_hash_buffer(pc, &regs),
        11_036_197_505_622_382_625,
    );
    assert_eq!(reg_hash(pc, &regs), 11_036_197_505_622_382_625);
}

#[test]
fn ram_hash_over_a_known_run_of_bytes() {
    let ram: Vec<u8> = (0..64u32).map(|i| (i % 256) as u8).collect();
    report("ram_hash", &ram, 17_854_084_224_570_037_232);
    assert_eq!(ram_hash(&ram), 17_854_084_224_570_037_232);
}

#[test]
fn fb_hash_over_a_framebuffer_then_a_palette() {
    let framebuffer: Vec<u8> = (0..16u8).collect();
    let palette: Vec<u8> = (200..208u8).collect();
    let mut joined = framebuffer.clone();
    joined.extend_from_slice(&palette);
    report("fb_hash", &joined, 10_814_741_248_291_066_246);
    assert_eq!(fb_hash(&framebuffer, &palette), 10_814_741_248_291_066_246);
}
