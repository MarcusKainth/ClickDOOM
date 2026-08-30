//! Capturing a machine and putting it back.
//!
//! The property that matters is the one a long run depends on: a run
//! interrupted and resumed reaches the same state as one that was never
//! interrupted. Everything else here is about refusing a file that would
//! resume into a machine that is quietly different.

use std::path::Path;

use clickdoom_spec::{IPMS_DEFAULT, RAM_BASE, TraceConfig};
use refemu::Cpu;
use refemu::asm::*;
use refemu::snapshot::{self, Kind, Provenance, Snapshot};
use refemu::trace::collect;

const FINE: TraceConfig = TraceConfig {
    checkpoint_interval: 4,
    ram_hash_interval: 16,
};

/// Counts down, storing as it goes, so RAM moves and the trace has content.
fn program_words() -> Vec<u32> {
    vec![
        addi(1, 0, 60),
        lui(2, 0x80001),
        addi(1, 1, -1),
        sw(2, 1, 0),
        lw(3, 2, 0),
        add(4, 4, 3),
        bne(1, 0, -16),
        ecall(),
    ]
}

fn fresh() -> Cpu {
    let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
    cpu.load_image(&program(&program_words()), RAM_BASE)
        .unwrap();
    cpu.set_text_region(Some((RAM_BASE, RAM_BASE + 32)));
    cpu.enable_decode_cache();
    cpu
}

fn nothing_known() -> Provenance {
    Provenance {
        rom_sha256: None,
        pinned: false,
        rom_manifest: None,
    }
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("refemu-snapshot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn a_run_interrupted_and_resumed_reaches_the_state_of_one_that_was_not() {
    let path = temp("resume.rsnap");

    // Uninterrupted.
    let mut whole = fresh();
    let (whole_lines, whole_stop) = collect(&mut whole, FINE, 10_000);

    // Interrupted at a checkpoint, captured, and carried on in a new machine.
    let mut first = fresh();
    let (mut lines, _) = collect(&mut first, FINE, 40);
    snapshot::machine_snapshot(&first, nothing_known(), Some("icount:40".to_owned()))
        .write(&path)
        .unwrap();

    let mut second = fresh();
    let saved = Snapshot::read(&path, &["ram", "framebuffer", "palette"]).unwrap();
    snapshot::restore(&mut second, &saved, &path, false).unwrap();
    second.enable_decode_cache();
    assert_eq!(second.icount(), 40);
    let (rest, stop) = collect(&mut second, FINE, 10_000);
    lines.extend(rest);

    assert_eq!(lines, whole_lines, "the resumed trace differs");
    assert_eq!(stop, whole_stop);
    assert_eq!(second.icount(), whole.icount());
    assert_eq!(second.regs(), whole.regs());
    assert_eq!(second.pc(), whole.pc());
    assert_eq!(second.memory.ram(), whole.memory.ram());
    assert!(!lines.is_empty(), "the run produced no trace to compare");
}

#[test]
fn a_capture_carries_the_device_state_a_resume_needs() {
    let path = temp("devices.rsnap");
    let mut cpu = fresh();
    {
        let registers = cpu.memory.devices_mut().registers_mut().unwrap();
        registers.console.extend_from_slice(b"hello");
        registers.push_key(true, 0x41);
        registers.push_key(false, 0x41);
        registers.frame_commits.push(refemu::FrameCommit {
            frame_no: 7,
            commit_icount: 3_000_000_000,
        });
    }
    snapshot::machine_snapshot(&cpu, nothing_known(), None)
        .write(&path)
        .unwrap();

    let mut back = fresh();
    let saved = Snapshot::read(&path, &[]).unwrap();
    snapshot::restore(&mut back, &saved, &path, false).unwrap();
    let registers = back.memory.devices().registers_ref().unwrap();
    assert_eq!(registers.console, b"hello");
    assert_eq!(registers.key_queue.len(), 2);
    assert_eq!(registers.key_queue[0].doomkey, 0x41);
    assert!(registers.key_queue[0].pressed);
    assert!(!registers.key_queue[1].pressed);
    // A count past four billion survives, which an eight-byte record would
    // not have room for.
    assert_eq!(registers.frame_commits[0].commit_icount, 3_000_000_000);
    assert_eq!(registers.frame_commits[0].frame_no, 7);
}

#[test]
fn a_frame_capture_carries_the_pixels_and_their_hash() {
    let path = temp("frame.rsnap");
    let mut cpu = fresh();
    cpu.memory
        .write(clickdoom_spec::FRAMEBUFFER_BASE, 4, 0x0403_0201, 0)
        .unwrap();
    let expected = format!("{:016x}", refemu::trace::fb_hash_of(&cpu));
    snapshot::frame_snapshot(&cpu, nothing_known(), None)
        .write(&path)
        .unwrap();

    let saved = Snapshot::read(&path, &["framebuffer", "palette"]).unwrap();
    assert_eq!(saved.header.kind, Kind::Frame);
    assert_eq!(saved.header.fbhash.as_deref(), Some(expected.as_str()));
    assert_eq!(saved.section("framebuffer").unwrap()[..4], [1, 2, 3, 4]);
    assert_eq!(saved.section("palette").unwrap().len(), 768);
    // A frame capture has no machine to resume, and says so by not carrying
    // the sections a resume asks for.
    assert!(saved.section("ram").is_none());
    assert!(Snapshot::read(&path, &["ram"]).is_err());
}

#[test]
fn a_file_that_is_not_a_snapshot_fails_on_its_first_bytes() {
    let path = temp("pickle.bin");
    // What the format this replaces looks like.
    std::fs::write(&path, b"\x80\x05\x95\x00\x00\x00\x00\x00\x00\x00\x00}").unwrap();
    let err = Snapshot::read(&path, &[]).unwrap_err().to_string();
    assert!(err.contains("not a refemu snapshot"), "{err}");
}

#[test]
fn a_snapshot_of_another_version_says_which() {
    let path = temp("v2.rsnap");
    std::fs::write(&path, b"REFEMU-SNAPSHOT 2\n{}\n").unwrap();
    let err = Snapshot::read(&path, &[]).unwrap_err().to_string();
    assert!(err.contains("version 2"), "{err}");
}

#[test]
fn a_section_that_was_cut_short_is_refused_rather_than_resumed_from() {
    let path = temp("corrupt.rsnap");
    let cpu = fresh();
    snapshot::machine_snapshot(&cpu, nothing_known(), None)
        .write(&path)
        .unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    // One byte of RAM, well past the header.
    let at = 4096 + 1024;
    bytes[at] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    let err = Snapshot::read(&path, &[]).unwrap_err().to_string();
    assert!(err.contains("does not match its own sha256"), "{err}");
}

#[test]
fn a_machine_captured_under_other_settings_is_refused_by_name() {
    let path = temp("other-ipms.rsnap");
    let mut cpu = Cpu::clickdoom(10);
    cpu.load_image(&program(&program_words()), RAM_BASE)
        .unwrap();
    snapshot::machine_snapshot(&cpu, nothing_known(), None)
        .write(&path)
        .unwrap();

    let mut ours = Cpu::clickdoom(IPMS_DEFAULT);
    let saved = Snapshot::read(&path, &[]).unwrap();
    let err = snapshot::restore(&mut ours, &saved, &path, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ipms"), "{err}");
    // The same refusal names the text region when that is what differs.
    let mut ours = Cpu::clickdoom(10);
    ours.set_text_region(Some((RAM_BASE, RAM_BASE + 8)));
    let err = snapshot::restore(&mut ours, &saved, &path, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("text region"), "{err}");
    // And forcing it through is allowed, deliberately.
    assert!(snapshot::restore(&mut ours, &saved, &path, true).is_ok());
}

#[test]
fn the_python_reader_reads_what_the_emulator_writes() {
    let path = temp("for-python.rsnap");
    let mut cpu = fresh();
    cpu.run_until_halt(1000).unwrap();
    {
        let registers = cpu.memory.devices_mut().registers_mut().unwrap();
        registers.console.extend_from_slice(b"abc");
    }
    snapshot::machine_snapshot(&cpu, nothing_known(), Some("halt".to_owned()))
        .write(&path)
        .unwrap();

    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("scripts");
    let output = std::process::Command::new("python3")
        .arg("-c")
        .arg(
            r#"
import sys
sys.path.insert(0, sys.argv[1])
from refemu_snapshot import load
header, sections = load(sys.argv[2], need=("ram", "framebuffer", "palette", "console"))
print(header["kind"], header["icount"], header["pc"], len(header["regs"]))
print(len(sections["ram"]), len(sections["framebuffer"]), len(sections["palette"]))
print(sections["console"].decode())
"#,
        )
        .arg(&script)
        .arg(&path)
        .output()
        .expect("python3 is on PATH");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        format!("machine {} {} 32", cpu.icount(), cpu.pc())
    );
    assert_eq!(
        lines[1],
        format!(
            "{} {} {}",
            cpu.memory.ram().len(),
            cpu.memory.framebuffer().len(),
            cpu.memory.palette().len()
        )
    );
    assert_eq!(lines[2], "abc");
}
