//! The committed traces, against what this interpreter produces now.
//!
//! Two tiers. The structural one needs nothing but the files, so a change to
//! the checkpoint format is caught in a fresh checkout with no ROM built. The
//! regeneration one needs the pinned ROM, and is behind the `rom-tests`
//! feature so that a run either includes it or visibly does not.
//!
//! Nothing here skips at runtime. A test reporting "ignored" is read by
//! nobody, and this repository has already had a job report success for
//! months without executing a comparison.

use std::path::{Path, PathBuf};

use clickdoom_spec::{Checkpoint, TraceConfig};
#[cfg(feature = "rom-tests")]
use clickdoom_spec::{IPMS_DEFAULT, Manifest, RAM_BASE, sha256_hex};
#[cfg(feature = "rom-tests")]
use refemu::Cpu;
#[cfg(feature = "rom-tests")]
use refemu::trace::{Stop, collect};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn traces_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("reference_traces")
}

/// The pinned ROM, or a failure naming what to run.
#[cfg(feature = "rom-tests")]
fn pinned_rom(what: &str) -> (Vec<u8>, Manifest, String) {
    let image = repo().join("rom").join("build").join("doom-rv32im.bin");
    assert!(
        image.exists(),
        "{what} needs {}, which is not built. Run `make build-rom`.",
        image.display()
    );
    let bytes = std::fs::read(&image).unwrap();
    let manifest = Manifest::read(&repo().join("rom").join("build").join("manifest.json")).unwrap();
    let pinned = std::fs::read_to_string(repo().join("rom").join("PINNED_HASH")).unwrap();
    let pinned = pinned.trim().to_owned();
    assert_eq!(
        sha256_hex(&bytes),
        pinned,
        "the built ROM is not the pinned one"
    );
    (bytes, manifest, pinned)
}

fn committed_traces() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(traces_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tsv"))
        .collect();
    found.sort();
    found
}

/// Every committed trace obeys the format, whether or not its ROM still
/// exists. The older ones cannot be regenerated, and they still say whether
/// the format moved.
#[test]
fn every_committed_trace_obeys_the_checkpoint_format() {
    let files = committed_traces();
    assert!(
        !files.is_empty(),
        "no committed traces under {}",
        traces_dir().display()
    );
    let config = TraceConfig::default();
    let mut checked = 0u64;

    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(text.ends_with('\n'), "{name} does not end with a newline");
        let mut expected_icount = config.checkpoint_interval;
        let mut memory_lines = 0u64;
        let mut lines = 0u64;

        for (number, line) in text.lines().enumerate() {
            let checkpoint: Checkpoint = line
                .parse()
                .unwrap_or_else(|e| panic!("{name}:{}: {e}", number + 1));
            assert_eq!(
                checkpoint.icount,
                expected_icount,
                "{name}:{}: the cadence skipped a checkpoint",
                number + 1
            );
            let at_memory = config.is_ram_hash(checkpoint.icount);
            assert_eq!(
                checkpoint.field_count(),
                if at_memory { 5 } else { 3 },
                "{name}:{}: a memory hash landed off its own cadence",
                number + 1
            );
            memory_lines += u64::from(at_memory);
            expected_icount += config.checkpoint_interval;
            lines += 1;
        }

        // The sidecar and the trace have to agree about what the trace is.
        let sidecar = path.with_extension("json");
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
        let recorded = meta
            .get("trace_line_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("{name}: its sidecar records no line count"));
        assert_eq!(recorded, lines, "{name}: the sidecar disagrees on length");
        assert!(memory_lines > 0, "{name} never reaches a memory hash");
        println!("{name}: {lines} lines, {memory_lines} with memory hashes");
        checked += lines;
    }
    assert!(checked > 1000, "only {checked} lines were checked");
}

/// The trace this interpreter produces now, against the committed one for the
/// pinned ROM. The filename is derived from the pin, so a re-pinned ROM with
/// no regenerated trace fails by name.
#[cfg(feature = "rom-tests")]
#[test]
fn the_committed_trace_regenerates_byte_for_byte() {
    let (image, manifest, pinned) = pinned_rom("the reference-trace comparison");
    let expected = traces_dir().join(format!("demo-boot-to-first-frame.{}.tsv", &pinned[..12]));
    assert!(
        expected.exists(),
        "no committed trace for the pinned ROM at {}. Run `make gen-reference-trace`.",
        expected.display()
    );

    let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
    cpu.load_image(&image, manifest.load_addr.unwrap_or(RAM_BASE))
        .unwrap();
    cpu.set_text_region(manifest.text_region());
    cpu.enable_decode_cache();

    let want = std::fs::read_to_string(&expected).unwrap();
    let budget = want.lines().count() as u64 * TraceConfig::default().checkpoint_interval;
    let (lines, stop) = collect(&mut cpu, TraceConfig::default(), budget);
    assert_eq!(stop, Stop::Budget, "the run stopped before the trace ends");

    let mut got = String::new();
    for line in &lines {
        got.push_str(&line.to_string());
        got.push('\n');
    }
    assert_eq!(got.len(), want.len(), "the traces are different lengths");
    if got != want {
        let at = got
            .lines()
            .zip(want.lines())
            .position(|(a, b)| a != b)
            .unwrap();
        panic!(
            "the traces differ first at line {}:\n  got  {}\n  want {}",
            at + 1,
            got.lines().nth(at).unwrap(),
            want.lines().nth(at).unwrap()
        );
    }
    println!(
        "{} lines byte-identical, {} of them carrying memory hashes",
        lines.len(),
        lines.iter().filter(|l| l.field_count() == 5).count()
    );
}
