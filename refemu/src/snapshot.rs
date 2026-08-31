//! A machine written to one file, and read back.
//!
//! One container serves two things a caller wants to keep: the whole machine,
//! so a run can start from the middle of another one, and a single frame's
//! pixels, so a render test has something to check against. They share a
//! format because they share a reader, and two formats mean two readers that
//! drift.
//!
//! ```text
//! byte 0      REFEMU-SNAPSHOT 3\n
//! byte 18     a one-line JSON header
//!             zeroes up to offset 4096
//! offset 4096 the sections, at the offsets the header states
//! ```
//!
//! The magic line is checked first, so a truncated file or a foreign format
//! fails on the first byte rather than deep inside a parse. Each section
//! carries its own sha256, so a file that was cut short is an error and not a
//! machine that quietly starts from the wrong state.
//!
//! The header describes the rest of the file, so a reader needs nothing from
//! this crate to take a container apart. Its `sections` list names every
//! section with the offset it starts at, the length it runs for and the
//! sha256 it must hash to. A reader seeks, takes the length, checks the hash.
//! Sections are found by name rather than by position, so a file carrying one
//! the reader has no use for still reads.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use clickdoom_spec::{Manifest, sha256_hex};
use serde::{Deserialize, Serialize};

use crate::exec::Cpu;
use crate::mmio::FrameCommit;

/// Bumped whenever a reader written against the previous number could be
/// wrong about a file carrying this one: a changed field meaning, a dropped
/// section, a changed framing. A new optional section does not bump it,
/// because readers ask for sections by name and fail when one they need is
/// absent.
pub const FORMAT_VERSION: u32 = 3;

/// Where the sections start. The RAM section is page-aligned so a reader can
/// map it.
const BODY_OFFSET: u64 = 4096;

fn magic() -> String {
    format!("REFEMU-SNAPSHOT {FORMAT_VERSION}\n")
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a refemu snapshot v{FORMAT_VERSION}")]
    NotASnapshot { path: PathBuf },
    #[error("{path} is version {found}, and this reads version {FORMAT_VERSION}")]
    WrongVersion { path: PathBuf, found: u32 },
    #[error("{path}: its header is not readable: {source}")]
    Header {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: section {section} is {found} bytes, and its header says {expected}")]
    ShortSection {
        path: PathBuf,
        section: String,
        found: usize,
        expected: usize,
    },
    #[error("{path}: section {section} does not match its own sha256")]
    Corrupt { path: PathBuf, section: String },
    #[error("{path} carries no section named {section}")]
    MissingSection { path: PathBuf, section: String },
    #[error(
        "{path} was taken with {field} {theirs}, and this run has {ours}. \
         Resuming would change what the machine does with no other sign."
    )]
    Mismatch {
        path: PathBuf,
        field: &'static str,
        theirs: String,
        ours: String,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Everything needed to carry on running.
    Machine,
    /// One frame's pixels.
    Frame,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SectionInfo {
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub sha256: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Provenance {
    pub rom_sha256: Option<String>,
    pub pinned: bool,
    pub rom_manifest: Option<Manifest>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FrameInfo {
    pub index: u64,
    pub frame_no: u32,
    pub commit_icount: u64,
    pub retired_icount: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Header {
    pub format_version: u32,
    pub kind: Kind,
    pub generator: String,
    pub created_from: Provenance,
    pub icount: u64,
    pub stop_condition: Option<String>,
    /// Machine snapshots only.
    pub pc: Option<u32>,
    pub regs: Option<Vec<u32>>,
    pub ram_base: Option<u32>,
    pub ram_size: Option<u32>,
    pub ipms: Option<u32>,
    pub text_start: Option<u32>,
    pub text_end: Option<u32>,
    pub frame_commit_count: u64,
    pub last_frame_commit: Option<FrameInfo>,
    /// Frame snapshots only.
    pub frame: Option<FrameInfo>,
    pub fbhash: Option<String>,
    pub sections: Vec<SectionInfo>,
}

/// A container in memory.
#[derive(Debug)]
pub struct Snapshot {
    pub header: Header,
    pub sections: Vec<(String, Vec<u8>)>,
}

impl Snapshot {
    pub fn section(&self, name: &str) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// Writes atomically: a temporary file beside the target, flushed, then
    /// renamed. A crash leaves the previous file or nothing, never half of a
    /// new one.
    pub fn write(&self, path: &Path) -> Result<(), SnapshotError> {
        let io = |source| SnapshotError::Io {
            path: path.to_owned(),
            source,
        };
        let mut header = self.header.clone();
        header.sections = Vec::new();
        let mut at = BODY_OFFSET;
        for (name, bytes) in &self.sections {
            header.sections.push(SectionInfo {
                name: name.clone(),
                offset: at,
                length: bytes.len() as u64,
                sha256: sha256_hex(bytes),
            });
            at += bytes.len() as u64;
        }

        let line = serde_json::to_string(&header).map_err(|e| io(e.into()))?;
        let mut head = magic().into_bytes();
        head.extend_from_slice(line.as_bytes());
        head.push(b'\n');
        if head.len() as u64 > BODY_OFFSET {
            return Err(io(std::io::Error::other(format!(
                "the header is {} bytes and the body starts at {BODY_OFFSET}",
                head.len()
            ))));
        }
        head.resize(BODY_OFFSET as usize, 0);

        let temp = path.with_extension(format!("tmp.{}", std::process::id()));
        {
            let mut file = std::fs::File::create(&temp).map_err(|source| SnapshotError::Io {
                path: temp.clone(),
                source,
            })?;
            file.write_all(&head).map_err(&io)?;
            for (_, bytes) in &self.sections {
                file.write_all(bytes).map_err(&io)?;
            }
            file.sync_all().map_err(&io)?;
        }
        std::fs::rename(&temp, path).map_err(&io)
    }

    /// Reads a container, checking every section against its own hash.
    ///
    /// `needed` names the sections the caller cannot do without, so a file
    /// that does not carry one fails here rather than reading as an empty
    /// region later.
    pub fn read(path: &Path, needed: &[&str]) -> Result<Self, SnapshotError> {
        let bytes = std::fs::read(path).map_err(|source| SnapshotError::Io {
            path: path.to_owned(),
            source,
        })?;
        let magic = magic();
        if bytes.len() < magic.len() || bytes[..magic.len()] != *magic.as_bytes() {
            // A version we do not read still says so, rather than reporting a
            // foreign format.
            if let Some(found) = version_of(&bytes)
                && found != FORMAT_VERSION
            {
                return Err(SnapshotError::WrongVersion {
                    path: path.to_owned(),
                    found,
                });
            }
            return Err(SnapshotError::NotASnapshot {
                path: path.to_owned(),
            });
        }
        let rest = &bytes[magic.len()..];
        let line_end =
            rest.iter()
                .position(|b| *b == b'\n')
                .ok_or_else(|| SnapshotError::NotASnapshot {
                    path: path.to_owned(),
                })?;
        let header: Header =
            serde_json::from_slice(&rest[..line_end]).map_err(|source| SnapshotError::Header {
                path: path.to_owned(),
                source,
            })?;

        let mut sections = Vec::new();
        for info in &header.sections {
            let at = info.offset as usize;
            let end = at + info.length as usize;
            let slice = bytes
                .get(at..end)
                .ok_or_else(|| SnapshotError::ShortSection {
                    path: path.to_owned(),
                    section: info.name.clone(),
                    found: bytes.len().saturating_sub(at),
                    expected: info.length as usize,
                })?;
            if sha256_hex(slice) != info.sha256 {
                return Err(SnapshotError::Corrupt {
                    path: path.to_owned(),
                    section: info.name.clone(),
                });
            }
            sections.push((info.name.clone(), slice.to_vec()));
        }

        let snapshot = Self { header, sections };
        for name in needed {
            if snapshot.section(name).is_none() {
                return Err(SnapshotError::MissingSection {
                    path: path.to_owned(),
                    section: (*name).to_owned(),
                });
            }
        }
        Ok(snapshot)
    }
}

/// The version a file claims, for a file this reader will not accept.
fn version_of(bytes: &[u8]) -> Option<u32> {
    let head = &bytes[..bytes.len().min(64)];
    let text = String::from_utf8_lossy(head);
    let rest = text.strip_prefix("REFEMU-SNAPSHOT ")?;
    rest.split('\n').next()?.trim().parse().ok()
}

fn frame_info(index: u64, commit: FrameCommit) -> FrameInfo {
    FrameInfo {
        index,
        frame_no: commit.frame_no,
        commit_icount: commit.commit_icount,
        retired_icount: commit.retired_icount(),
    }
}

fn base_header(cpu: &Cpu, kind: Kind, from: Provenance, stop: Option<String>) -> Header {
    let commits: Vec<FrameCommit> = cpu
        .memory
        .devices()
        .registers_ref()
        .map(|r| r.frame_commits.clone())
        .unwrap_or_default();
    Header {
        format_version: FORMAT_VERSION,
        kind,
        generator: concat!("refemu ", env!("CARGO_PKG_VERSION")).to_owned(),
        created_from: from,
        icount: cpu.icount(),
        stop_condition: stop,
        pc: None,
        regs: None,
        ram_base: None,
        ram_size: None,
        ipms: None,
        text_start: None,
        text_end: None,
        frame_commit_count: commits.len() as u64,
        last_frame_commit: commits
            .last()
            .map(|c| frame_info(commits.len() as u64 - 1, *c)),
        frame: None,
        fbhash: None,
        sections: Vec::new(),
    }
}

/// Captures the whole machine.
pub fn machine_snapshot(cpu: &Cpu, from: Provenance, stop: Option<String>) -> Snapshot {
    let map = *cpu.memory.map();
    let registers = cpu.memory.devices().registers_ref();
    let mut header = base_header(cpu, Kind::Machine, from, stop);
    header.pc = Some(cpu.pc());
    header.regs = Some(cpu.regs().to_vec());
    header.ram_base = Some(map.ram_base);
    header.ram_size = Some(map.ram_size);
    header.ipms = registers.map(|r| r.ipms());
    if let Some((start, end)) = cpu.memory.text_region() {
        header.text_start = Some(start);
        header.text_end = Some(end);
    }

    let mut keyq = Vec::new();
    let mut commits = Vec::new();
    let mut console = Vec::new();
    if let Some(registers) = registers {
        for event in &registers.key_queue {
            keyq.push(event.pressed as u8);
            keyq.push(event.doomkey);
        }
        for commit in &registers.frame_commits {
            commits.extend_from_slice(&commit.frame_no.to_le_bytes());
            commits.extend_from_slice(&commit.commit_icount.to_le_bytes());
        }
        console = registers.console.clone();
    }

    Snapshot {
        header,
        sections: vec![
            ("ram".to_owned(), cpu.memory.ram().to_vec()),
            ("framebuffer".to_owned(), cpu.memory.framebuffer().to_vec()),
            ("palette".to_owned(), cpu.memory.palette().to_vec()),
            ("console".to_owned(), console),
            ("keyq".to_owned(), keyq),
            ("frame_commits".to_owned(), commits),
        ],
    }
}

/// Captures one frame's pixels and the hash over them.
pub fn frame_snapshot(cpu: &Cpu, from: Provenance, stop: Option<String>) -> Snapshot {
    let commits: Vec<FrameCommit> = cpu
        .memory
        .devices()
        .registers_ref()
        .map(|r| r.frame_commits.clone())
        .unwrap_or_default();
    let mut header = base_header(cpu, Kind::Frame, from, stop);
    header.frame = commits
        .last()
        .map(|c| frame_info(commits.len() as u64 - 1, *c));
    header.fbhash = Some(format!("{:016x}", crate::trace::fb_hash_of(cpu)));
    Snapshot {
        header,
        sections: vec![
            ("framebuffer".to_owned(), cpu.memory.framebuffer().to_vec()),
            ("palette".to_owned(), cpu.memory.palette().to_vec()),
        ],
    }
}

/// Puts a machine snapshot back, refusing one taken under settings that would
/// make the resumed run behave differently.
pub fn restore(
    cpu: &mut Cpu,
    snapshot: &Snapshot,
    path: &Path,
    force: bool,
) -> Result<(), SnapshotError> {
    let header = &snapshot.header;
    let mismatch = |field, theirs: String, ours: String| SnapshotError::Mismatch {
        path: path.to_owned(),
        field,
        theirs,
        ours,
    };
    if !force {
        let map = *cpu.memory.map();
        if header.ram_size != Some(map.ram_size) {
            return Err(mismatch(
                "ram_size",
                format!("{:?}", header.ram_size),
                map.ram_size.to_string(),
            ));
        }
        let ours = cpu.memory.devices().registers_ref().map(|r| r.ipms());
        if header.ipms != ours {
            return Err(mismatch(
                "ipms",
                format!("{:?}", header.ipms),
                format!("{ours:?}"),
            ));
        }
        let theirs = match (header.text_start, header.text_end) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        };
        if theirs != cpu.memory.text_region() {
            return Err(mismatch(
                "the text region",
                format!("{theirs:?}"),
                format!("{:?}", cpu.memory.text_region()),
            ));
        }
    }

    let need = |name: &str| {
        snapshot
            .section(name)
            .ok_or_else(|| SnapshotError::MissingSection {
                path: path.to_owned(),
                section: name.to_owned(),
            })
    };
    cpu.restore_memory(need("ram")?, need("framebuffer")?, need("palette")?);
    let mut regs = [0u32; 32];
    if let Some(saved) = &header.regs {
        for (slot, value) in regs.iter_mut().zip(saved) {
            *slot = *value;
        }
    }
    cpu.restore(header.pc.unwrap_or_default(), regs, header.icount);

    if let Some(registers) = cpu.memory.devices_mut().registers_mut() {
        registers.console = need("console")?.to_vec();
        registers.key_queue.clear();
        for pair in need("keyq")?.as_chunks::<2>().0 {
            registers.push_key(pair[0] != 0, pair[1]);
        }
        registers.frame_commits.clear();
        // Twelve bytes each: a frame number and the count the device saw,
        // which is 64 bits here as it is everywhere else.
        for row in need("frame_commits")?.as_chunks::<12>().0 {
            registers.frame_commits.push(FrameCommit {
                frame_no: u32::from_le_bytes([row[0], row[1], row[2], row[3]]),
                commit_icount: u64::from_le_bytes([
                    row[4], row[5], row[6], row[7], row[8], row[9], row[10], row[11],
                ]),
            });
        }
    }
    Ok(())
}
