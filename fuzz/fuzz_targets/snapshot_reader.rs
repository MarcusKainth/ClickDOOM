//! A malformed capture is an error, and a good one round-trips.
//!
//! The reader takes a file it did not write, and a capture that reads back
//! wrong would resume into a machine that is quietly different rather than
//! failing.
#![no_main]

use libfuzzer_sys::fuzz_target;
use refemu::snapshot::Snapshot;

fuzz_target!(|data: &[u8]| {
    let dir = std::env::temp_dir().join(format!("refemu-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("case.rsnap");
    std::fs::write(&path, data).unwrap();

    let Ok(snapshot) = Snapshot::read(&path, &[]) else {
        return;
    };
    // Anything that read back has to describe itself consistently, and has to
    // survive being written and read again.
    assert_eq!(snapshot.header.format_version, refemu::snapshot::FORMAT_VERSION);
    for info in &snapshot.header.sections {
        assert_eq!(
            snapshot.section(&info.name).map(<[u8]>::len),
            Some(info.length as usize)
        );
    }
    let again = dir.join("again.rsnap");
    snapshot.write(&again).unwrap();
    let back = Snapshot::read(&again, &[]).expect("a capture this reader wrote is unreadable");
    assert_eq!(back.header.icount, snapshot.header.icount);
    assert_eq!(back.sections.len(), snapshot.sections.len());
    for (name, bytes) in &snapshot.sections {
        assert_eq!(back.section(name), Some(bytes.as_slice()));
    }
});
