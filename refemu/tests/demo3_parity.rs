//! The whole `-timedemo demo3` run, against what the manifest records.
//!
//! This is the strongest check available: 2.3 billion instructions, ending on
//! the frame hash the Definition of Victory names. It reads the manifest and
//! never writes it, so it cannot pass by comparing a run against itself. The
//! target that writes one is `make gen-demo3-trace`, and the two share no
//! code path.
//!
//! The trace is streamed through a hash rather than written, so this needs no
//! twenty megabytes of disk to say whether it agrees.
//!
//! Behind the `rom-tests` feature, so a run either includes it or visibly
//! does not.
#![cfg(feature = "rom-tests")]

use std::path::{Path, PathBuf};

use clickdoom_spec::{
    Checkpoint, IPMS_DEFAULT, Manifest, RAM_BASE, Sha256Stream, TraceConfig, sha256_hex,
};
use refemu::trace::{Observer, Stop};
use refemu::{Cpu, trace};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Hashes the trace as it goes, and keeps the last line and the counts.
#[derive(Default)]
struct Hasher {
    digest: Sha256Stream,
    bytes: u64,
    lines: u64,
    last: Option<String>,
}

impl Observer for Hasher {
    fn checkpoint(&mut self, checkpoint: Checkpoint) {
        let line = format!("{checkpoint}\n");
        self.digest.update(line.as_bytes());
        self.bytes += line.len() as u64;
        self.lines += 1;
        self.last = Some(line[..line.len() - 1].to_owned());
    }
}

#[test]
fn the_whole_demo_reproduces_what_the_manifest_records() {
    let image_path = repo().join("rom").join("build").join("doom-rv32im.bin");
    assert!(
        image_path.exists(),
        "the demo3 comparison needs {}, which is not built. Run `make build-rom`.",
        image_path.display()
    );
    let image = std::fs::read(&image_path).unwrap();
    let pinned = std::fs::read_to_string(repo().join("rom").join("PINNED_HASH")).unwrap();
    let pinned = pinned.trim();
    assert_eq!(
        sha256_hex(&image),
        pinned,
        "the built ROM is not the pinned one"
    );

    let recorded_path = repo()
        .join("refemu")
        .join("reference_traces")
        .join("demo3")
        .join(format!("demo3.{}.json", &pinned[..12]));
    assert!(
        recorded_path.exists(),
        "no demo3 manifest for the pinned ROM at {}. Run `make gen-demo3-trace`.",
        recorded_path.display()
    );
    let recorded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&recorded_path).unwrap()).unwrap();

    let manifest = Manifest::read(&repo().join("rom").join("build").join("manifest.json")).unwrap();
    let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
    cpu.load_image(&image, manifest.load_addr.unwrap_or(RAM_BASE))
        .unwrap();
    cpu.set_text_region(manifest.text_region());
    cpu.enable_decode_cache();

    let mut hasher = Hasher::default();
    let stop = trace::run(&mut cpu, TraceConfig::default(), u64::MAX, &mut hasher);
    let Stop::Halted(halt) = stop else {
        panic!("the demo did not halt: {stop:?}");
    };

    let want = |key: &str| -> u64 {
        recorded
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("the manifest records no {key}"))
    };
    let want_str = |key: &str| -> String {
        recorded
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("the manifest records no {key}"))
            .to_owned()
    };

    // Each field on its own, because one mismatched digest says nothing about
    // where to look.
    assert_eq!(cpu.icount(), want("final_icount"), "final icount");
    assert_eq!(
        hasher.digest.finish(),
        want_str("trace_file_sha256"),
        "the trace's own sha256"
    );
    assert_eq!(hasher.bytes, want("trace_file_bytes"), "trace size");
    assert_eq!(hasher.lines, want("trace_line_count"), "trace line count");
    assert_eq!(
        hasher.last.as_deref(),
        Some(want_str("final_checkpoint_line").as_str()),
        "the last checkpoint line"
    );

    let final_state = recorded.get("final_state").unwrap();
    let state = |key: &str| final_state.get(key).unwrap().as_str().unwrap();
    assert_eq!(
        format!("{:016x}", trace::reg_hash_of(&cpu)),
        state("reghash")
    );
    assert_eq!(
        format!("{:016x}", trace::ram_hash_of(&cpu)),
        state("ramhash")
    );
    assert_eq!(format!("{:016x}", trace::fb_hash_of(&cpu)), state("fbhash"));

    let recorded_halt = recorded.get("halt").unwrap();
    assert_eq!(
        halt.reason.to_string(),
        recorded_halt["reason"].as_str().unwrap()
    );
    assert_eq!(u64::from(halt.pc), recorded_halt["pc"].as_u64().unwrap());
    assert_eq!(
        halt.exit_code.map(u64::from),
        recorded_halt["exit_code"].as_u64()
    );

    let registers = cpu.memory.devices().registers_ref().unwrap();
    assert_eq!(
        registers.frame_commits.len() as u64,
        want("frame_commit_count"),
        "frame commit count"
    );
    let last = registers.frame_commits.last().unwrap();
    let recorded_last = recorded.get("last_frame_commit").unwrap();
    assert_eq!(
        u64::from(last.frame_no),
        recorded_last["frame_no"].as_u64().unwrap()
    );
    assert_eq!(
        last.commit_icount,
        recorded_last["commit_icount"].as_u64().unwrap()
    );

    println!(
        "{} instructions, {} checkpoint lines, sha256 {} matched, final fbhash {}",
        cpu.icount(),
        hasher.lines,
        &want_str("trace_file_sha256")[..16],
        state("fbhash")
    );
}
