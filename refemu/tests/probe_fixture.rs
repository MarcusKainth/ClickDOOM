//! The committed probe fixture, against what the probe produces now.
//!
//! Two tiers, the way `reference_trace.rs` has them. The structural one needs
//! nothing but the files, so a change to the row shape is caught in a fresh
//! checkout with no ROM built. The regeneration one needs the pinned ROM and
//! is behind the `rom-tests` feature, so a run either includes it or visibly
//! does not.

use std::path::{Path, PathBuf};

fn probe_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("probe")
}

/// The committed fixture, whichever ROM it came from.
fn fixture() -> PathBuf {
    let mut found: Vec<PathBuf> = std::fs::read_dir(probe_dir().join("fixtures"))
        .expect("refemu/probe/fixtures/ is missing")
        .map(|entry| entry.expect("reading refemu/probe/fixtures/").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tsv"))
        .collect();
    found.sort();
    assert_eq!(
        found.len(),
        1,
        "one fixture per ROM, and a re-pinned ROM replaces it: {found:?}"
    );
    found.remove(0)
}

fn rows(text: &str) -> Vec<&str> {
    text.lines().filter(|line| !line.starts_with('#')).collect()
}

fn columns(text: &str) -> Vec<&str> {
    text.lines()
        .find_map(|line| line.strip_prefix("# columns\t"))
        .expect("the fixture has no column header")
        .split('\t')
        .collect()
}

#[test]
fn the_fixture_names_the_contract_columns_in_order() {
    let text = std::fs::read_to_string(fixture()).unwrap();
    let columns = columns(&text);
    assert_eq!(&columns[..3], &["frame_index", "gametic", "fb_hash"]);
    assert_eq!(
        &columns[3..],
        clickdoom_spec::native_state::all_fields().as_slice(),
        "the fixture and the contract disagree on the column list"
    );
    assert!(
        text.contains(&format!(
            "# state_schema_version\t{}",
            clickdoom_spec::native_state::STATE_SCHEMA_VERSION
        )),
        "the fixture was written against a different schema version"
    );
}

#[test]
fn every_fixture_row_has_one_value_per_column() {
    let text = std::fs::read_to_string(fixture()).unwrap();
    let width = columns(&text).len();
    let rows = rows(&text);
    assert!(!rows.is_empty(), "the fixture has no rows");
    let mut last: Option<u64> = None;
    for row in &rows {
        let values: Vec<&str> = row.split('\t').collect();
        assert_eq!(values.len(), width, "row {} is the wrong width", values[0]);
        for value in &values {
            assert!(!value.is_empty(), "an empty value in row {}", values[0]);
            if value.starts_with('[') {
                assert!(
                    value.ends_with(']'),
                    "a truncated array in row {}",
                    values[0]
                );
            }
        }
        // Frame indices ascend, so a selection cannot have written a frame
        // twice or out of order.
        let index: u64 = values[0].parse().expect("frame_index is not a count");
        assert!(
            last.is_none_or(|last| last < index),
            "frame {index} repeats"
        );
        last = Some(index);
    }
}

#[test]
fn the_fixture_stays_small_enough_to_commit() {
    let bytes = std::fs::metadata(fixture()).unwrap().len();
    assert!(bytes < 512 * 1024, "the fixture is {bytes} bytes");
}

/// The fixture is what the probe writes now, byte for byte.
///
/// `make gen-probe-fixture` regenerates it, and the frames it names are the
/// ones the Makefile names.
#[cfg(feature = "rom-tests")]
#[test]
fn regenerating_the_fixture_reproduces_it() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let elf = repo.join("rom").join("build").join("doom-rv32im.elf");
    assert!(
        elf.exists(),
        "this needs {}, which is not built. Run `make build-rom`.",
        elf.display()
    );

    let frames = std::fs::read_to_string(repo.join("Makefile"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("PROBE_FIXTURE_FRAMES ?= "))
        .expect("the Makefile no longer names the fixture's frames")
        .trim()
        .to_owned();

    let image = refemu::Image::parse_elf(&std::fs::read(&elf).unwrap()).unwrap();
    let layout = refemu::probe::Layout::parse(
        &std::fs::read_to_string(probe_dir().join("layout.tsv")).unwrap(),
    )
    .unwrap();

    let manifest =
        clickdoom_spec::Manifest::read(&repo.join("rom").join("build").join("manifest.json"))
            .unwrap();
    let map = clickdoom_spec::MemoryMap::clickdoom();
    let mut cpu = refemu::Cpu::new(
        refemu::Memory::new(
            map,
            refemu::Devices::registers(clickdoom_spec::IPMS_DEFAULT),
        ),
        image.entry,
    );
    cpu.load(&image).unwrap();
    cpu.set_text_region(manifest.text_region());
    cpu.enable_decode_cache();

    let mut out = refemu::probe::header().into_bytes();
    let mut probe = refemu::probe::Probe::new(
        &image,
        &layout,
        frames.parse().expect("the Makefile's frame selection"),
        &mut out,
    )
    .unwrap();
    refemu::trace::run(
        &mut cpu,
        clickdoom_spec::TraceConfig::default(),
        4_000_000_000,
        &mut probe,
    );
    assert_eq!(probe.failed, None, "the probe stopped on an error");

    let committed = std::fs::read(fixture()).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        String::from_utf8(committed).unwrap(),
        "the fixture is stale. Run `make gen-probe-fixture`."
    );
}
