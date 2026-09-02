//! One frame of game state, as one TSV row.
//!
//! The column order is `clickdoom_spec::native_state`'s and nothing else's.
//! Every write names the column it is for and is checked against that list in
//! order, so a field written out of turn is an error rather than a file the
//! parity query silently misreads.
//!
//! Arrays are written in ClickHouse's TSV array syntax, `[a,b,c]`.

use std::fmt::Display;
use std::fmt::Write as _;

use clickdoom_spec::XXH64_SEED;
use clickdoom_spec::native_state::{
    ANIM_FIELDS, BUTTON_FIELDS, GAME_FIELDS, HUD_FIELDS, INPUT_FIELDS, LINE_SIDE_FIELDS,
    MOBJ_FIELDS, PLAYER_FIELDS, PSPRITE_FIELDS, SECTOR_FIELDS, SECTOR_THINKER_FIELDS,
};
use xxhash_rust::xxh64::xxh64;

use crate::exec::Cpu;
use crate::trace::fb_hash_of;

use super::ProbeError;
use super::ram::Ram;
use super::world::{self, Engine, Thinker, ThinkerKind, Walk};

/// A message longer than this is not one the engine wrote.
const MESSAGE_LIMIT: u32 = 1024;

/// Identity columns the SQL side counts and the probe cannot read.
///
/// `refemu/probe/README.md` says why, and the parity query drops them.
const UNREADABLE: i64 = 0;

/// Builds a row, checking each column against the contract's order.
struct Row {
    out: String,
    want: Vec<&'static str>,
    at: usize,
}

impl Row {
    fn new() -> Self {
        Self {
            out: String::with_capacity(4096),
            want: clickdoom_spec::native_state::all_fields(),
            at: 0,
        }
    }

    /// Writes one column, naming which it is.
    fn put(&mut self, name: &'static str, value: impl Display) -> Result<(), ProbeError> {
        match self.want.get(self.at) {
            Some(want) if *want == name => {}
            want => {
                return Err(ProbeError::ColumnOutOfOrder {
                    wrote: name.to_owned(),
                    want: want.map_or("nothing", |want| *want).to_owned(),
                });
            }
        }
        self.at += 1;
        self.out.push('\t');
        let _ = write!(self.out, "{value}");
        Ok(())
    }

    /// Writes one array column, in ClickHouse's TSV array syntax.
    fn put_array<T: Display>(
        &mut self,
        name: &'static str,
        values: impl IntoIterator<Item = T>,
    ) -> Result<(), ProbeError> {
        self.put(name, "")?;
        self.out.push('[');
        for (index, value) in values.into_iter().enumerate() {
            if index > 0 {
                self.out.push(',');
            }
            let _ = write!(self.out, "{value}");
        }
        self.out.push(']');
        Ok(())
    }

    fn finish(self) -> Result<String, ProbeError> {
        if self.at != self.want.len() {
            return Err(ProbeError::RowTooShort {
                wrote: self.at,
                total: self.want.len(),
            });
        }
        let mut out = self.out;
        out.push('\n');
        Ok(out)
    }
}

/// Reads one struct's fields into a row of parallel arrays.
///
/// Each column is a separate pass over the thinkers, because the contract
/// stores one array per field rather than one record per thinker.
fn column<T: Display>(
    thinkers: &[Thinker],
    read: impl FnMut(&Thinker) -> Result<T, ProbeError>,
) -> Result<Vec<T>, ProbeError> {
    thinkers.iter().map(read).collect()
}

/// Everything one frame needs, read once so each column does not re-walk.
struct Frame<'a> {
    engine: &'a Engine,
    ram: Ram<'a>,
    walk: Walk,
    player: u32,
    numsectors: i32,
    numlines: i32,
    numsides: i32,
    numsubsectors: i32,
}

impl Frame<'_> {
    /// A pointer to a mobj as its one-based slot, 0 for none.
    fn mobj_slot(&self, addr: u32) -> u32 {
        self.walk.mobj_slot(addr)
    }

    /// A pointer into an array of engine records as its index, -1 for NULL.
    fn index(
        &self,
        base_ptr: u32,
        count: i32,
        stride: u32,
        ptr: u32,
        what: &'static str,
        into: &'static str,
    ) -> Result<i32, ProbeError> {
        if ptr == 0 {
            return Ok(-1);
        }
        let base = self.ram.u32(base_ptr, what)?;
        world::index_of(base, count.max(0) as u32, stride, ptr, what, into)
    }

    /// A mobj's player pointer as a player number, -1 when it has none.
    fn player_number(&self, ptr: u32) -> Result<i32, ProbeError> {
        if ptr == 0 {
            return Ok(-1);
        }
        let players = self.engine.globals.players;
        world::index_of(
            players.addr,
            players.count,
            self.engine.offsets.player.size,
            ptr,
            "mobj.player",
            "player",
        )
    }

    fn state_index(&self, ptr: u32, what: &'static str) -> Result<i32, ProbeError> {
        if ptr == 0 {
            return Ok(-1);
        }
        world::index_of(
            self.engine.globals.states.addr,
            self.engine.globals.states.count,
            self.engine.offsets.state_size,
            ptr,
            what,
            "state",
        )
    }

    /// The xxh64 of a NUL-terminated string in RAM, 0 for a null pointer.
    fn message_hash(&self, ptr: u32) -> Result<u64, ProbeError> {
        if ptr == 0 {
            return Ok(0);
        }
        Ok(xxh64(
            self.ram.cstr(ptr, MESSAGE_LIMIT, "a message")?,
            XXH64_SEED,
        ))
    }
}

/// The whole row for the frame the machine has just committed.
pub fn write(engine: &Engine, cpu: &Cpu, frame_index: u64) -> Result<String, ProbeError> {
    let ram = world::ram_of(cpu);
    let frame = Frame {
        walk: world::walk(engine, cpu)?,
        player: engine.globals.players.addr,
        numsectors: ram.i32(engine.globals.numsectors, "numsectors")?,
        numlines: ram.i32(engine.globals.numlines, "numlines")?,
        numsides: ram.i32(engine.globals.numsides, "numsides")?,
        numsubsectors: ram.i32(engine.globals.numsubsectors, "numsubsectors")?,
        engine,
        ram,
    };

    let mut row = Row::new();
    row.out.push_str(&frame_index.to_string());
    let _ = write!(
        row.out,
        "\t{}\t{:016x}",
        frame.ram.i32(engine.globals.gametic, "gametic")?,
        fb_hash_of(cpu)
    );

    game(&mut row, &frame)?;
    mobjs(&mut row, &frame)?;
    sector_thinkers(&mut row, &frame)?;
    sectors(&mut row, &frame)?;
    lines_and_sides(&mut row, &frame)?;
    buttons(&mut row, &frame)?;
    anims(&mut row, &frame)?;
    player(&mut row, &frame)?;
    psprites(&mut row, &frame)?;
    hud(&mut row, &frame)?;
    input(&mut row, &frame)?;
    row.finish()
}

fn game(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let g = &frame.engine.globals;
    let ram = &frame.ram;
    let mut at = GAME_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    row.put(next(), ram.i32(g.leveltime, "leveltime")?)?;
    row.put(next(), ram.i32(g.prndindex, "prndindex")?)?;
    row.put(next(), ram.i32(g.rndindex, "rndindex")?)?;
    row.put(next(), UNREADABLE)?;
    row.put(next(), UNREADABLE)?;
    row.put(next(), ram.u32(g.paused, "paused")?)?;
    // The demo has ended once the engine has stopped playing one back.
    row.put(
        next(),
        u32::from(ram.u32(g.demoplayback, "demoplayback")? == 0),
    )?;
    row.put(next(), ram.i32(g.totalkills, "totalkills")?)?;
    row.put(next(), ram.i32(g.totalitems, "totalitems")?)?;
    row.put(next(), ram.i32(g.totalsecret, "totalsecret")?)?;
    Ok(())
}

fn mobjs(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let o = &frame.engine.offsets.mobj;
    let sp = &frame.engine.offsets.mapthing;
    let ram = &frame.ram;
    let list = &frame.walk.mobjs;
    let mut at = MOBJ_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");

    row.put_array(next(), list.iter().map(|_| UNREADABLE))?;
    for offset in [o.x, o.y, o.z] {
        row.put_array(
            next(),
            column(list, |t| ram.i32(t.addr + offset, "a mobj"))?,
        )?;
    }
    row.put_array(
        next(),
        column(list, |t| ram.u32(t.addr + o.angle, "a mobj"))?,
    )?;
    for offset in [o.sprite, o.frame, o.floorz, o.ceilingz, o.radius, o.height] {
        row.put_array(
            next(),
            column(list, |t| ram.i32(t.addr + offset, "a mobj"))?,
        )?;
    }
    for offset in [o.momx, o.momy, o.momz, o.kind, o.tics] {
        row.put_array(
            next(),
            column(list, |t| ram.i32(t.addr + offset, "a mobj"))?,
        )?;
    }
    row.put_array(
        next(),
        column(list, |t| {
            frame.state_index(ram.u32(t.addr + o.state, "mobj.state")?, "mobj.state")
        })?,
    )?;
    for offset in [o.flags, o.health, o.movedir, o.movecount] {
        row.put_array(
            next(),
            column(list, |t| ram.i32(t.addr + offset, "a mobj"))?,
        )?;
    }
    row.put_array(
        next(),
        column(list, |t| {
            Ok(frame.mobj_slot(ram.u32(t.addr + o.target, "mobj.target")?))
        })?,
    )?;
    for offset in [o.reactiontime, o.threshold] {
        row.put_array(
            next(),
            column(list, |t| ram.i32(t.addr + offset, "a mobj"))?,
        )?;
    }
    row.put_array(
        next(),
        column(list, |t| {
            frame.player_number(ram.u32(t.addr + o.player, "mobj.player")?)
        })?,
    )?;
    row.put_array(
        next(),
        column(list, |t| ram.i32(t.addr + o.lastlook, "mobj.lastlook"))?,
    )?;
    for offset in [sp.x, sp.y, sp.angle, sp.kind, sp.options] {
        row.put_array(
            next(),
            column(list, |t| {
                ram.i16(t.addr + o.spawnpoint + offset, "mobj.spawnpoint")
            })?,
        )?;
    }
    row.put_array(
        next(),
        column(list, |t| {
            Ok(frame.mobj_slot(ram.u32(t.addr + o.tracer, "mobj.tracer")?))
        })?,
    )?;
    row.put_array(
        next(),
        column(list, |t| {
            frame.index(
                frame.engine.globals.subsectors,
                frame.numsubsectors,
                frame.engine.offsets.subsector_size,
                ram.u32(t.addr + o.subsector, "mobj.subsector")?,
                "mobj.subsector",
                "subsector",
            )
        })?,
    )?;
    row.put_array(next(), list.iter().map(|_| UNREADABLE))?;
    Ok(())
}

/// One sector thinker's fields, in the order the contract names them.
///
/// A field a kind does not have is 0. The mapping from each engine struct to
/// this shared set of columns is in `refemu/probe/README.md`.
struct SectorThinkerRow {
    sector: i32,
    kind: u8,
    values: [i32; 17],
    active: u32,
    plat_slot: u32,
    ceiling_slot: u32,
}

/// Positions in `SectorThinkerRow::values`, in contract order.
mod slot {
    pub const TYPE: usize = 0;
    pub const DIRECTION: usize = 1;
    pub const SPEED: usize = 2;
    pub const DEST: usize = 3;
    pub const DEST2: usize = 4;
    pub const COUNT: usize = 5;
    pub const WAIT: usize = 6;
    pub const STATUS: usize = 7;
    pub const OLDSTATUS: usize = 8;
    pub const CRUSH: usize = 9;
    pub const TAG: usize = 10;
    pub const TEXTURE: usize = 11;
    pub const NEWSPECIAL: usize = 12;
    pub const MINLIGHT: usize = 13;
    pub const MAXLIGHT: usize = 14;
    pub const MINTIME: usize = 15;
    pub const MAXTIME: usize = 16;
}

fn sector_thinkers(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let rows: Vec<SectorThinkerRow> = frame
        .walk
        .sector_thinkers
        .iter()
        .map(|t| sector_thinker(frame, t))
        .collect::<Result<_, _>>()?;

    let mut at = SECTOR_THINKER_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    row.put_array(next(), rows.iter().map(|_| UNREADABLE))?;
    row.put_array(next(), rows.iter().map(|r| r.kind))?;
    row.put_array(next(), rows.iter().map(|r| r.sector))?;
    for index in [
        slot::TYPE,
        slot::DIRECTION,
        slot::SPEED,
        slot::DEST,
        slot::DEST2,
        slot::COUNT,
        slot::WAIT,
        slot::STATUS,
        slot::OLDSTATUS,
        slot::CRUSH,
        slot::TAG,
        slot::TEXTURE,
        slot::NEWSPECIAL,
        slot::MINLIGHT,
        slot::MAXLIGHT,
        slot::MINTIME,
        slot::MAXTIME,
    ] {
        row.put_array(next(), rows.iter().map(|r| r.values[index]))?;
    }
    row.put_array(next(), rows.iter().map(|r| r.active))?;
    row.put_array(next(), rows.iter().map(|r| r.plat_slot))?;
    row.put_array(next(), rows.iter().map(|r| r.ceiling_slot))?;
    Ok(())
}

fn sector_thinker(frame: &Frame<'_>, t: &Thinker) -> Result<SectorThinkerRow, ProbeError> {
    let ram = &frame.ram;
    let o = &frame.engine.offsets;
    let mut values = [0i32; 17];
    let at = t.addr;
    let sector_ptr;
    let mut set = |index: usize, value: i32| values[index] = value;

    match t.kind {
        ThinkerKind::Mobj => return Err(ProbeError::MobjAsSectorThinker(at)),
        ThinkerKind::Door => {
            let d = &o.door;
            sector_ptr = ram.u32(at + d.sector, "vldoor.sector")?;
            set(slot::TYPE, ram.i32(at + d.kind, "vldoor.type")?);
            set(slot::DEST, ram.i32(at + d.topheight, "vldoor.topheight")?);
            set(slot::SPEED, ram.i32(at + d.speed, "vldoor.speed")?);
            set(slot::DIRECTION, ram.i32(at + d.direction, "vldoor")?);
            set(slot::WAIT, ram.i32(at + d.topwait, "vldoor.topwait")?);
            set(slot::COUNT, ram.i32(at + d.topcountdown, "vldoor")?);
        }
        ThinkerKind::Plat => {
            let p = &o.plat;
            sector_ptr = ram.u32(at + p.sector, "plat.sector")?;
            set(slot::TYPE, ram.i32(at + p.kind, "plat.type")?);
            set(slot::SPEED, ram.i32(at + p.speed, "plat.speed")?);
            set(slot::DEST, ram.i32(at + p.low, "plat.low")?);
            set(slot::DEST2, ram.i32(at + p.high, "plat.high")?);
            set(slot::WAIT, ram.i32(at + p.wait, "plat.wait")?);
            set(slot::COUNT, ram.i32(at + p.count, "plat.count")?);
            set(slot::STATUS, ram.i32(at + p.status, "plat.status")?);
            set(slot::OLDSTATUS, ram.i32(at + p.oldstatus, "plat")?);
            set(slot::CRUSH, ram.i32(at + p.crush, "plat.crush")?);
            set(slot::TAG, ram.i32(at + p.tag, "plat.tag")?);
        }
        ThinkerKind::Floor => {
            let f = &o.floor;
            sector_ptr = ram.u32(at + f.sector, "floormove.sector")?;
            set(slot::TYPE, ram.i32(at + f.kind, "floormove.type")?);
            set(slot::CRUSH, ram.i32(at + f.crush, "floormove.crush")?);
            set(slot::DIRECTION, ram.i32(at + f.direction, "floormove")?);
            set(slot::NEWSPECIAL, ram.i32(at + f.newspecial, "floormove")?);
            set(slot::TEXTURE, ram.i16(at + f.texture, "floormove")? as i32);
            set(slot::DEST, ram.i32(at + f.floordestheight, "floormove")?);
            set(slot::SPEED, ram.i32(at + f.speed, "floormove.speed")?);
        }
        ThinkerKind::Ceiling => {
            let c = &o.ceiling;
            sector_ptr = ram.u32(at + c.sector, "ceiling.sector")?;
            set(slot::TYPE, ram.i32(at + c.kind, "ceiling.type")?);
            set(slot::DEST, ram.i32(at + c.bottomheight, "ceiling")?);
            set(slot::DEST2, ram.i32(at + c.topheight, "ceiling")?);
            set(slot::SPEED, ram.i32(at + c.speed, "ceiling.speed")?);
            set(slot::CRUSH, ram.i32(at + c.crush, "ceiling.crush")?);
            set(slot::DIRECTION, ram.i32(at + c.direction, "ceiling")?);
            set(slot::TAG, ram.i32(at + c.tag, "ceiling.tag")?);
            set(slot::OLDSTATUS, ram.i32(at + c.olddirection, "ceiling")?);
        }
        ThinkerKind::LightFlash => {
            let l = &o.light_flash;
            sector_ptr = ram.u32(at + l.sector, "lightflash.sector")?;
            set(slot::COUNT, ram.i32(at + l.count, "lightflash.count")?);
            set(slot::MAXLIGHT, ram.i32(at + l.maxlight, "lightflash")?);
            set(slot::MINLIGHT, ram.i32(at + l.minlight, "lightflash")?);
            set(slot::MAXTIME, ram.i32(at + l.maxtime, "lightflash")?);
            set(slot::MINTIME, ram.i32(at + l.mintime, "lightflash")?);
        }
        ThinkerKind::Strobe => {
            let s = &o.strobe;
            sector_ptr = ram.u32(at + s.sector, "strobe.sector")?;
            set(slot::COUNT, ram.i32(at + s.count, "strobe.count")?);
            set(slot::MINLIGHT, ram.i32(at + s.minlight, "strobe")?);
            set(slot::MAXLIGHT, ram.i32(at + s.maxlight, "strobe")?);
            set(slot::MINTIME, ram.i32(at + s.darktime, "strobe")?);
            set(slot::MAXTIME, ram.i32(at + s.brighttime, "strobe")?);
        }
        ThinkerKind::Glow => {
            let g = &o.glow;
            sector_ptr = ram.u32(at + g.sector, "glow.sector")?;
            set(slot::MINLIGHT, ram.i32(at + g.minlight, "glow")?);
            set(slot::MAXLIGHT, ram.i32(at + g.maxlight, "glow")?);
            set(slot::DIRECTION, ram.i32(at + g.direction, "glow")?);
        }
        ThinkerKind::FireFlicker => {
            let f = &o.fire_flicker;
            sector_ptr = ram.u32(at + f.sector, "fireflicker.sector")?;
            set(slot::COUNT, ram.i32(at + f.count, "fireflicker.count")?);
            set(slot::MAXLIGHT, ram.i32(at + f.maxlight, "fireflicker")?);
            set(slot::MINLIGHT, ram.i32(at + f.minlight, "fireflicker")?);
        }
    }

    Ok(SectorThinkerRow {
        sector: frame.index(
            frame.engine.globals.sectors,
            frame.numsectors,
            frame.engine.offsets.sector.size,
            sector_ptr,
            "a sector thinker's sector",
            "sector",
        )?,
        kind: t.kind.sector_kind(),
        values,
        active: u32::from(t.active),
        plat_slot: table_slot(
            frame,
            frame.engine.globals.activeplats,
            t.addr,
            "activeplats",
        )?,
        ceiling_slot: table_slot(
            frame,
            frame.engine.globals.activeceilings,
            t.addr,
            "activeceilings",
        )?,
    })
}

/// The one-based position of a thinker in one of the engine's active tables,
/// 0 when it is not in it.
fn table_slot(
    frame: &Frame<'_>,
    table: world::Global,
    addr: u32,
    what: &'static str,
) -> Result<u32, ProbeError> {
    for index in 0..table.count {
        if frame.ram.u32(table.addr + index * 4, what)? == addr {
            return Ok(index + 1);
        }
    }
    Ok(0)
}

fn sectors(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let o = &frame.engine.offsets.sector;
    let ram = &frame.ram;
    let base = ram.u32(frame.engine.globals.sectors, "sectors")?;
    let count = frame.numsectors.max(0) as u32;
    let each = |offset: u32| (0..count).map(move |i| base + i * o.size + offset);

    let mut at = SECTOR_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    for offset in [o.floorheight, o.ceilingheight] {
        row.put_array(
            next(),
            each(offset)
                .map(|a| ram.i32(a, "a sector"))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
    }
    for offset in [o.floorpic, o.lightlevel, o.special] {
        row.put_array(
            next(),
            each(offset)
                .map(|a| ram.i16(a, "a sector"))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
    }
    row.put_array(
        next(),
        each(o.specialdata)
            .map(|a| Ok(frame.walk.sector_slot(ram.u32(a, "sector.specialdata")?)))
            .collect::<Result<Vec<u32>, ProbeError>>()?,
    )?;
    row.put_array(
        next(),
        each(o.soundtarget)
            .map(|a| Ok(frame.mobj_slot(ram.u32(a, "sector.soundtarget")?)))
            .collect::<Result<Vec<u32>, ProbeError>>()?,
    )?;
    row.put_array(
        next(),
        each(o.soundtraversed)
            .map(|a| ram.i32(a, "sector.soundtraversed"))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    Ok(())
}

fn lines_and_sides(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let ram = &frame.ram;
    let line = &frame.engine.offsets.line;
    let side = &frame.engine.offsets.side;
    let lines = ram.u32(frame.engine.globals.lines, "lines")?;
    let sides = ram.u32(frame.engine.globals.sides, "sides")?;
    let numlines = frame.numlines.max(0) as u32;
    let numsides = frame.numsides.max(0) as u32;

    let mut at = LINE_SIDE_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    row.put_array(
        next(),
        (0..numlines)
            .map(|i| ram.i16(lines + i * line.size + line.special, "line.special"))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    for offset in [side.toptexture, side.midtexture, side.bottomtexture] {
        row.put_array(
            next(),
            (0..numsides)
                .map(|i| ram.i16(sides + i * side.size + offset, "a side"))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
    }
    row.put_array(
        next(),
        (0..numsides)
            .map(|i| ram.i32(sides + i * side.size + side.textureoffset, "a side"))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    Ok(())
}

fn buttons(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let ram = &frame.ram;
    let o = &frame.engine.offsets.button;
    let table = frame.engine.globals.buttonlist;
    let each = |offset: u32| (0..table.count).map(move |i| table.addr + i * o.size + offset);

    let mut at = BUTTON_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    row.put_array(
        next(),
        each(o.line)
            .map(|a| {
                frame.index(
                    frame.engine.globals.lines,
                    frame.numlines,
                    frame.engine.offsets.line.size,
                    ram.u32(a, "button.line")?,
                    "button.line",
                    "line",
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    for offset in [o.place, o.btexture, o.btimer] {
        row.put_array(
            next(),
            each(offset)
                .map(|a| ram.i32(a, "a button"))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
    }
    Ok(())
}

fn anims(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let ram = &frame.ram;
    let g = &frame.engine.globals;
    let mut at = ANIM_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    for (table, count, what) in [
        (g.texturetranslation, g.numtextures, "texturetranslation"),
        (g.flattranslation, g.numflats, "flattranslation"),
    ] {
        let base = ram.u32(table, what)?;
        let count = ram.i32(count, what)?.max(0) as u32;
        row.put_array(
            next(),
            (0..count)
                .map(|i| ram.i32(base + i * 4, what))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
    }
    Ok(())
}

fn player(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let ram = &frame.ram;
    let o = &frame.engine.offsets.player;
    let cmd = &frame.engine.offsets.ticcmd;
    let at_player = frame.player;
    let words = |field: super::layout::ArrayField| {
        (0..field.count)
            .map(|i| ram.i32(at_player + field.offset + i * 4, "a player array"))
            .collect::<Result<Vec<_>, _>>()
    };

    let mut at = PLAYER_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    row.put(
        next(),
        frame.mobj_slot(ram.u32(at_player + o.mo, "player.mo")?),
    )?;
    row.put(next(), ram.i32(at_player + o.playerstate, "player")?)?;
    row.put(next(), ram.i8(at_player + o.cmd + cmd.forwardmove, "cmd")?)?;
    row.put(next(), ram.i8(at_player + o.cmd + cmd.sidemove, "cmd")?)?;
    row.put(next(), ram.i16(at_player + o.cmd + cmd.angleturn, "cmd")?)?;
    row.put(next(), ram.u8(at_player + o.cmd + cmd.buttons, "cmd")?)?;
    for offset in [
        o.viewz,
        o.viewheight,
        o.deltaviewheight,
        o.bob,
        o.health,
        o.armorpoints,
        o.armortype,
    ] {
        row.put(next(), ram.i32(at_player + offset, "a player field")?)?;
    }
    row.put_array(next(), words(o.powers)?)?;
    row.put_array(next(), words(o.cards)?)?;
    row.put(next(), ram.i32(at_player + o.backpack, "player")?)?;
    row.put(next(), ram.i32(at_player + o.readyweapon, "player")?)?;
    row.put(next(), ram.i32(at_player + o.pendingweapon, "player")?)?;
    row.put_array(next(), words(o.weaponowned)?)?;
    row.put_array(next(), words(o.ammo)?)?;
    row.put_array(next(), words(o.maxammo)?)?;
    for offset in [
        o.attackdown,
        o.usedown,
        o.cheats,
        o.refire,
        o.killcount,
        o.itemcount,
        o.secretcount,
    ] {
        row.put(next(), ram.i32(at_player + offset, "a player field")?)?;
    }
    row.put(
        next(),
        frame.message_hash(ram.u32(at_player + o.message, "player.message")?)?,
    )?;
    row.put(next(), ram.i32(at_player + o.damagecount, "player")?)?;
    row.put(next(), ram.i32(at_player + o.bonuscount, "player")?)?;
    row.put(
        next(),
        frame.mobj_slot(ram.u32(at_player + o.attacker, "player.attacker")?),
    )?;
    row.put(next(), ram.i32(at_player + o.extralight, "player")?)?;
    row.put(next(), ram.i32(at_player + o.fixedcolormap, "player")?)?;
    Ok(())
}

fn psprites(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let ram = &frame.ram;
    let o = &frame.engine.offsets.player.psprites;
    let p = &frame.engine.offsets.pspdef;
    let stride = frame.engine.offsets.pspdef.size;
    let base = frame.player + o.offset;
    let each = |offset: u32| (0..o.count).map(move |i| base + i * stride + offset);

    let mut at = PSPRITE_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    row.put_array(
        next(),
        each(p.state)
            .map(|a| frame.state_index(ram.u32(a, "psprite.state")?, "psprite.state"))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    for offset in [p.tics, p.sx, p.sy] {
        row.put_array(
            next(),
            each(offset)
                .map(|a| ram.i32(a, "a psprite"))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
    }
    Ok(())
}

fn hud(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let ram = &frame.ram;
    let g = &frame.engine.globals;
    let mut at = HUD_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    for (addr, what) in [
        (g.st_faceindex, "st_faceindex"),
        (g.st_facecount, "st_facecount"),
        (g.st_priority, "priority"),
        (g.st_lastattackdown, "lastattackdown"),
    ] {
        row.put(next(), ram.i32(addr, what)?)?;
    }
    row.put_array(
        next(),
        (0..g.st_oldweaponsowned.count)
            .map(|i| ram.i32(g.st_oldweaponsowned.addr + i * 4, "oldweaponsowned"))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    for (addr, what) in [
        (g.st_oldhealth, "st_oldhealth"),
        (g.st_randomnumber, "st_randomnumber"),
        (g.st_lastcalc, "lastcalc"),
        (g.st_calc_oldhealth, "oldhealth"),
        (g.st_palette, "st_palette"),
        (g.st_clock, "st_clock"),
        (g.message_on, "message_on"),
        (g.message_counter, "message_counter"),
    ] {
        row.put(next(), ram.i32(addr, what)?)?;
    }
    row.put(next(), hud_message(frame)?)?;
    row.put(
        next(),
        ram.i32(g.message_nottobefuckedwith, "message_nottobefuckedwith")?,
    )?;
    row.put(next(), ram.i16(g.skull_anim_counter, "skullAnimCounter")?)?;
    row.put(next(), ram.i16(g.which_skull, "whichSkull")?)?;
    Ok(())
}

/// The xxh64 of the line the message widget is showing, 0 when it is empty.
fn hud_message(frame: &Frame<'_>) -> Result<u64, ProbeError> {
    let ram = &frame.ram;
    let stext = &frame.engine.offsets.stext;
    let textline = &frame.engine.offsets.textline;
    let widget = frame.engine.globals.w_message;
    let current = ram.i32(widget + stext.current, "w_message.cl")?;
    if current < 0 || current as u32 >= stext.line_count {
        return Err(ProbeError::NotAnIndex {
            what: "w_message.cl",
            addr: widget,
            value: current as u32,
            into: "message line",
        });
    }
    let line = widget + stext.lines + current as u32 * textline.size;
    let len = ram.i32(line + textline.len, "a message line's length")?;
    let len = len.clamp(0, textline.text.count as i32) as u32;
    if len == 0 {
        return Ok(0);
    }
    let text = ram.cstr(line + textline.text.offset, len, "a message line")?;
    Ok(xxh64(text, XXH64_SEED))
}

fn input(row: &mut Row, frame: &Frame<'_>) -> Result<(), ProbeError> {
    let mut at = INPUT_FIELDS.iter();
    let mut next = || at.next().copied().unwrap_or("");
    row.put(
        next(),
        frame.ram.i32(frame.engine.globals.turnheld, "turnheld")?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_writes_the_contract_columns_in_order() {
        let mut row = Row::new();
        let fields = clickdoom_spec::native_state::all_fields();
        for name in &fields {
            row.put(name, 0).unwrap();
        }
        let text = row.finish().unwrap();
        assert_eq!(text.matches('\t').count(), fields.len());
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_column_written_out_of_turn_is_an_error_naming_both() {
        let mut row = Row::new();
        let err = row.put("m_x", 0).unwrap_err();
        assert!(err.to_string().contains("m_x"), "{err}");
        assert!(err.to_string().contains("leveltime"), "{err}");
    }

    #[test]
    fn a_row_that_stops_early_is_an_error_rather_than_a_short_line() {
        let mut row = Row::new();
        row.put("leveltime", 1).unwrap();
        let err = row.finish().unwrap_err();
        assert!(
            matches!(err, ProbeError::RowTooShort { wrote: 1, .. }),
            "{err}"
        );
    }

    #[test]
    fn an_array_column_is_written_in_clickhouse_tsv_syntax() {
        let mut row = Row::new();
        for name in clickdoom_spec::native_state::GAME_FIELDS {
            row.put(name, 0).unwrap();
        }
        row.put_array("m_id", [1, 2, 3]).unwrap();
        assert!(row.out.ends_with("\t[1,2,3]"), "{}", row.out);
        row.put_array("m_x", Vec::<i32>::new()).unwrap();
        assert!(row.out.ends_with("\t[]"), "{}", row.out);
    }
}
