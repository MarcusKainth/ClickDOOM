//! The `probe` subcommand.
//!
//! One pass over the ROM, writing one row of game state per frame commit. It
//! takes the ELF rather than the flat binary, because the state is read by
//! symbol name and only the ELF carries the symbol table.

use std::path::{Path, PathBuf};

use clap::Args;
use clickdoom_spec::{
    Manifest, MemoryMap, RAM_BASE, Sha256Stream, TraceConfig, hashed_filename, sha256_hex,
};
use serde::Serialize;

use crate::exec::Cpu;
use crate::image::Image;
use crate::memory::Memory;
use crate::mmio::Devices;
use crate::probe::{self, Frames, Layout, Probe};
use crate::trace;

use super::point::{StopAt, parse_count};
use super::{Exit, Failure, MachineArgs, RunOutcome, failed, gate, usage, write_json};

#[derive(Args, Clone)]
pub struct ProbeCmd {
    /// The ELF the ROM was linked from, which is what carries the symbols
    pub image: PathBuf,
    /// Manifest holding load_addr, text_start and text_end
    #[arg(long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,
    /// Refuse to run unless the image's segments flatten to this hash
    #[arg(long, value_name = "PATH")]
    pub pinned_hash: Option<PathBuf>,
    /// The struct layout table
    #[arg(long, value_name = "PATH")]
    pub layout: PathBuf,
    #[command(flatten)]
    pub machine: MachineArgs,
    /// Instruction budget
    #[arg(long, short = 'n', value_name = "N", default_value_t = 4_000_000_000, value_parser = parse_count)]
    pub max_instructions: u64,
    /// Stop here. Repeatable, and the earliest one wins
    #[arg(long = "stop-at", value_name = "COND")]
    pub stop_at: Vec<StopAt>,
    /// Write only these frames, as indices and `a..b` ranges
    #[arg(long, value_name = "FRAMES")]
    pub frames: Option<Frames>,
    /// Write the rows here instead of stdout
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
    /// Write the rows into this directory, named after the ROM's sha256
    #[arg(long, value_name = "DIR", requires = "name", conflicts_with = "out")]
    pub out_dir: Option<PathBuf>,
    /// Filename stem used with --out-dir
    #[arg(long, value_name = "STEM")]
    pub name: Option<String>,
    /// Write what the run recorded about itself. With --out-dir it defaults
    /// beside the rows
    #[arg(long, value_name = "PATH")]
    pub meta_out: Option<PathBuf>,
}

pub const PROBE_META_SCHEMA: &str = "refemu.probe-meta/1";

/// What a probe run recorded about itself.
#[derive(Serialize)]
pub struct ProbeMeta {
    pub schema: String,
    pub refemu_version: String,
    pub state_schema_version: u32,
    pub rom_sha256: String,
    pub layout_sha256: String,
    pub probe_file: Option<String>,
    pub probe_file_sha256: String,
    pub probe_file_bytes: u64,
    pub probe_row_count: u64,
    pub frame_commit_count: u64,
    pub first_gameplay_frame: Option<u64>,
    pub stop_condition: Option<String>,
    pub final_icount: u64,
    pub halted: bool,
    pub elapsed_seconds: u64,
    pub elapsed_millis: u32,
    pub instructions_per_second: u64,
}

/// The bytes `objcopy -O binary` would produce from these segments: every
/// loadable segment's file content, at its address, with the gaps zeroed.
///
/// The ELF file's own hash is not stable across builds. The compiler puts the
/// name of its temporary object file in `.strtab`, and that name is random.
/// The flattened segments are the ROM, and they are what `PINNED_HASH` covers.
fn flatten(image: &Image) -> Vec<u8> {
    let Some(lo) = image.segments.iter().map(|s| s.vaddr).min() else {
        return Vec::new();
    };
    let hi = image
        .segments
        .iter()
        .map(|s| s.vaddr as u64 + s.bytes.len() as u64)
        .max()
        .unwrap_or(lo as u64);
    let mut out = vec![0u8; (hi - lo as u64) as usize];
    for segment in &image.segments {
        let at = (segment.vaddr - lo) as usize;
        out[at..at + segment.bytes.len()].copy_from_slice(&segment.bytes);
    }
    out
}

/// Runs the probe and writes what it was asked to write.
pub(crate) fn run(quiet: bool, cmd: &ProbeCmd) -> Result<Exit, Failure> {
    let bytes = std::fs::read(&cmd.image)
        .map_err(|e| failed(format!("reading {}: {e}", cmd.image.display())))?;
    let image = Image::parse_elf(&bytes).map_err(|e| failed(e.to_string()))?;

    // The pinned hash covers the ROM, and the ROM is the flattened segments.
    let flat = flatten(&image);
    let rom_sha256 = sha256_hex(&flat);
    if let Some(path) = &cmd.pinned_hash {
        let pinned = std::fs::read_to_string(path)
            .map_err(|e| failed(format!("reading {}: {e}", path.display())))?;
        let pinned = pinned.trim();
        if pinned != rom_sha256 {
            return Err(gate(format!(
                "the image flattens to {rom_sha256}, and {} pins {pinned}",
                path.display()
            )));
        }
    }

    let layout_bytes = std::fs::read(&cmd.layout)
        .map_err(|e| failed(format!("reading {}: {e}", cmd.layout.display())))?;
    let layout_sha256 = sha256_hex(&layout_bytes);
    let layout_text = String::from_utf8(layout_bytes)
        .map_err(|_| failed(format!("{} is not text", cmd.layout.display())))?;
    let layout = Layout::parse(&layout_text).map_err(|e| failed(e.to_string()))?;

    let manifest = match &cmd.manifest {
        Some(path) => Manifest::read(path).map_err(|e| failed(e.to_string()))?,
        None => Manifest::default(),
    };

    let out_path = match (&cmd.out, &cmd.out_dir, &cmd.name) {
        (Some(path), _, _) => Some(path.clone()),
        (None, Some(dir), Some(name)) => {
            let file =
                hashed_filename(name, &rom_sha256, ".tsv").map_err(|e| failed(e.to_string()))?;
            std::fs::create_dir_all(dir)
                .map_err(|e| failed(format!("creating {}: {e}", dir.display())))?;
            Some(dir.join(file))
        }
        _ => None,
    };
    let meta_path = cmd
        .meta_out
        .clone()
        .or_else(|| match (&cmd.out_dir, &out_path) {
            (Some(_), Some(path)) => Some(path.with_extension("json")),
            _ => None,
        });

    let mut cpu = load(&image, &manifest, &cmd.machine)?;

    // Hashed as it is written, so a file of any size still has a hash and is
    // never read back to find out what it holds.
    let sink = Sink::new(out_path.as_deref())?;
    let mut probe = Probe::new(
        &image,
        &layout,
        cmd.frames.clone().unwrap_or_else(Frames::all),
        sink,
    )
    .map_err(|e| failed(e.to_string()))?;

    // purity-ok: reported in the metadata, never read by the machine.
    let started = std::time::Instant::now();
    let stop = trace::run(
        &mut cpu,
        TraceConfig::default(),
        cmd.max_instructions,
        &mut probe,
    );
    let elapsed = started.elapsed();

    if let Some(error) = probe.failed.clone() {
        return Err(failed(error.to_string()));
    }
    let written = probe.written.clone();
    let sink = probe.into_sink();
    let (digest, bytes_written) = sink.finish()?;

    let halted = matches!(stop, trace::Stop::Halted(_));
    let outcome = match stop {
        trace::Stop::Halted(_) => RunOutcome::Halt,
        trace::Stop::Budget => RunOutcome::Budget,
        trace::Stop::Observer => RunOutcome::Stop,
    };
    let wanted: Vec<crate::cli::point::Point> = cmd.stop_at.iter().map(|s| s.0).collect();
    let stop_condition = match outcome {
        RunOutcome::Halt if wanted.contains(&crate::cli::point::Point::Halt) => Some("halt"),
        RunOutcome::Budget if wanted.contains(&crate::cli::point::Point::Budget) => Some("budget"),
        RunOutcome::Stop => Some("frames"),
        _ => None,
    };

    if !quiet {
        eprintln!(
            "# wrote {} rows over {} frame commits in {}.{:03}s (icount={})",
            written.rows,
            written.frames_seen,
            elapsed.as_secs(),
            elapsed.subsec_millis(),
            cpu.icount()
        );
    }

    if let Some(path) = &meta_path {
        let meta = ProbeMeta {
            schema: PROBE_META_SCHEMA.to_owned(),
            refemu_version: env!("CARGO_PKG_VERSION").to_owned(),
            state_schema_version: clickdoom_spec::native_state::STATE_SCHEMA_VERSION,
            rom_sha256: rom_sha256.clone(),
            layout_sha256,
            probe_file: out_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            probe_file_sha256: digest,
            probe_file_bytes: bytes_written,
            probe_row_count: written.rows,
            frame_commit_count: written.frames_seen,
            first_gameplay_frame: written.first_gameplay_frame,
            stop_condition: stop_condition.map(str::to_owned),
            final_icount: cpu.icount(),
            halted,
            elapsed_seconds: elapsed.as_secs(),
            elapsed_millis: elapsed.subsec_millis(),
            instructions_per_second: (cpu.icount() as f64 / elapsed.as_secs_f64()) as u64,
        };
        write_json(path, &meta).map_err(|e| failed(format!("writing {}: {e}", path.display())))?;
    }

    Ok(match outcome {
        RunOutcome::Stop => Exit::Ok,
        RunOutcome::Halt if stop_condition.is_some() => Exit::Ok,
        RunOutcome::Halt => Exit::Halted,
        RunOutcome::Budget if stop_condition.is_some() => Exit::Ok,
        RunOutcome::Budget => Exit::Budget,
    })
}

/// Loads the ELF into a machine, the way `run` and `trace` load theirs.
fn load(image: &Image, manifest: &Manifest, machine: &MachineArgs) -> Result<Cpu, Failure> {
    let map = MemoryMap::clickdoom().with_ram_size(machine.ram_size);
    let mut cpu = Cpu::new(
        Memory::new(map, Devices::registers(machine.ipms)),
        image.entry,
    );
    cpu.load(image).map_err(|e| failed(e.to_string()))?;
    cpu.set_text_region(match (manifest.text_start, manifest.text_end) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => image.text_region(),
    });
    if manifest.load_addr.is_some_and(|addr| addr != RAM_BASE) {
        return Err(usage(
            "the probe runs the ClickDOOM map, whose RAM starts at RAM_BASE".to_owned(),
        ));
    }
    cpu.enable_decode_cache();
    Ok(cpu)
}

/// Where the rows go, hashed as they are written.
struct Sink {
    out: Box<dyn std::io::Write>,
    digest: Sha256Stream,
    bytes: u64,
    error: Option<String>,
}

impl Sink {
    fn new(path: Option<&Path>) -> Result<Self, Failure> {
        let out: Box<dyn std::io::Write> = match path {
            Some(path) if path != Path::new("-") => Box::new(std::io::BufWriter::new(
                std::fs::File::create(path)
                    .map_err(|e| failed(format!("creating {}: {e}", path.display())))?,
            )),
            _ => Box::new(std::io::BufWriter::new(std::io::stdout())),
        };
        let mut sink = Self {
            out,
            digest: Sha256Stream::new(),
            bytes: 0,
            error: None,
        };
        use std::io::Write as _;
        let header = probe::header();
        sink.write_all(header.as_bytes())
            .map_err(|e| failed(format!("writing the header: {e}")))?;
        Ok(sink)
    }

    fn finish(mut self) -> Result<(String, u64), Failure> {
        self.out
            .flush()
            .map_err(|e| failed(format!("flushing the rows: {e}")))?;
        if let Some(error) = self.error {
            return Err(failed(format!("writing the rows: {error}")));
        }
        Ok((self.digest.finish(), self.bytes))
    }
}

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.digest.update(buf);
        self.bytes += buf.len() as u64;
        if self.error.is_none()
            && let Err(e) = self.out.write_all(buf)
        {
            self.error = Some(e.to_string());
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}
