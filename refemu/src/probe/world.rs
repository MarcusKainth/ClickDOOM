//! The engine as the probe addresses it: where each global lives, where each
//! field sits, and what the thinker list holds this frame.
//!
//! Everything here is resolved once, before the run, from the image's symbol
//! table and the layout table. A name the image does not carry is an error at
//! that point rather than a wrong number two hours into a run.

use std::collections::HashMap;

use crate::exec::Cpu;
use crate::image::{Image, SymbolKind};

use super::ProbeError;
use super::layout::{Layout, offsets};
use super::ram::Ram;

/// A thinker list longer than this is a corrupt one rather than a level.
const THINKER_LIMIT: u32 = 1 << 20;

/// `P_RemoveThinker` marks a thinker dead by putting this in its function
/// slot. It stays on the list until `P_RunThinkers` unlinks it.
const THINKER_REMOVED: u32 = 0xFFFF_FFFF;

/// A thinker with no function is in stasis: still on the list, not running.
const THINKER_STASIS: u32 = 0;

offsets! {
    /// The list links every thinker starts with.
    ThinkerOffsets from "thinker_t" {
        prev: 4,
        next: 4,
        function: 4,
    }
}

offsets! {
    /// A thing's entry in the map, kept for nightmare respawn.
    MapthingOffsets from "mapthing_t" {
        x: 2,
        y: 2,
        angle: 2,
        kind as "type": 2,
        options: 2,
    }
}

offsets! {
    MobjOffsets from "mobj_t" {
        x: 4,
        y: 4,
        z: 4,
        angle: 4,
        sprite: 4,
        frame: 4,
        subsector: 4,
        floorz: 4,
        ceilingz: 4,
        radius: 4,
        height: 4,
        momx: 4,
        momy: 4,
        momz: 4,
        kind as "type": 4,
        tics: 4,
        state: 4,
        flags: 4,
        health: 4,
        movedir: 4,
        movecount: 4,
        target: 4,
        reactiontime: 4,
        threshold: 4,
        player: 4,
        lastlook: 4,
        spawnpoint: 10,
        tracer: 4,
    }
}

offsets! {
    TiccmdOffsets from "ticcmd_t" {
        forwardmove: 1,
        sidemove: 1,
        angleturn: 2,
        buttons: 1,
    }
}

offsets! {
    PspdefOffsets from "pspdef_t" {
        state: 4,
        tics: 4,
        sx: 4,
        sy: 4,
    }
}

offsets! {
    PlayerOffsets from "player_t" {
        mo: 4,
        playerstate: 4,
        cmd: 16,
        viewz: 4,
        viewheight: 4,
        deltaviewheight: 4,
        bob: 4,
        health: 4,
        armorpoints: 4,
        armortype: 4,
        powers: [4],
        cards: [4],
        backpack: 4,
        readyweapon: 4,
        pendingweapon: 4,
        weaponowned: [4],
        ammo: [4],
        maxammo: [4],
        attackdown: 4,
        usedown: 4,
        cheats: 4,
        refire: 4,
        killcount: 4,
        itemcount: 4,
        secretcount: 4,
        message: 4,
        damagecount: 4,
        bonuscount: 4,
        attacker: 4,
        extralight: 4,
        fixedcolormap: 4,
        psprites: [16],
    }
}

offsets! {
    SectorOffsets from "sector_t" {
        floorheight: 4,
        ceilingheight: 4,
        floorpic: 2,
        lightlevel: 2,
        special: 2,
        soundtraversed: 4,
        soundtarget: 4,
        specialdata: 4,
    }
}

offsets! {
    LineOffsets from "line_t" {
        special: 2,
    }
}

offsets! {
    SideOffsets from "side_t" {
        textureoffset: 4,
        toptexture: 2,
        bottomtexture: 2,
        midtexture: 2,
    }
}

offsets! {
    DoorOffsets from "vldoor_t" {
        kind as "type": 4,
        sector: 4,
        topheight: 4,
        speed: 4,
        direction: 4,
        topwait: 4,
        topcountdown: 4,
    }
}

offsets! {
    PlatOffsets from "plat_t" {
        sector: 4,
        speed: 4,
        low: 4,
        high: 4,
        wait: 4,
        count: 4,
        status: 4,
        oldstatus: 4,
        crush: 4,
        tag: 4,
        kind as "type": 4,
    }
}

offsets! {
    FloorOffsets from "floormove_t" {
        kind as "type": 4,
        crush: 4,
        sector: 4,
        direction: 4,
        newspecial: 4,
        texture: 2,
        floordestheight: 4,
        speed: 4,
    }
}

offsets! {
    CeilingOffsets from "ceiling_t" {
        kind as "type": 4,
        sector: 4,
        bottomheight: 4,
        topheight: 4,
        speed: 4,
        crush: 4,
        direction: 4,
        tag: 4,
        olddirection: 4,
    }
}

offsets! {
    LightFlashOffsets from "lightflash_t" {
        sector: 4,
        count: 4,
        maxlight: 4,
        minlight: 4,
        maxtime: 4,
        mintime: 4,
    }
}

offsets! {
    StrobeOffsets from "strobe_t" {
        sector: 4,
        count: 4,
        minlight: 4,
        maxlight: 4,
        darktime: 4,
        brighttime: 4,
    }
}

offsets! {
    GlowOffsets from "glow_t" {
        sector: 4,
        minlight: 4,
        maxlight: 4,
        direction: 4,
    }
}

offsets! {
    FireFlickerOffsets from "fireflicker_t" {
        sector: 4,
        count: 4,
        maxlight: 4,
        minlight: 4,
    }
}

offsets! {
    ButtonOffsets from "button_t" {
        line: 4,
        place as "where": 4,
        btexture: 4,
        btimer: 4,
    }
}

offsets! {
    TextlineOffsets from "hu_textline_t" {
        text as "l": [1],
        len: 4,
    }
}

/// The scrolling message widget. Resolved by hand rather than through the
/// macro, because how many lines it holds is its `l` extent divided by the
/// size of a line, and both come from the table.
pub struct StextOffsets {
    pub size: u32,
    pub lines: u32,
    pub line_count: u32,
    pub current: u32,
}

impl StextOffsets {
    fn resolve(layout: &Layout, line_size: u32) -> Result<Self, ProbeError> {
        let (lines, line_count) = layout.array("hu_stext_t", "l", line_size)?;
        Ok(Self {
            size: layout.size_of("hu_stext_t")?,
            lines,
            line_count,
            current: layout.field("hu_stext_t", "cl", 4)?,
        })
    }
}

/// Every offset the probe reads.
pub struct Offsets {
    pub thinker: ThinkerOffsets,
    pub mapthing: MapthingOffsets,
    pub mobj: MobjOffsets,
    pub ticcmd: TiccmdOffsets,
    pub pspdef: PspdefOffsets,
    pub player: PlayerOffsets,
    pub sector: SectorOffsets,
    pub line: LineOffsets,
    pub side: SideOffsets,
    pub door: DoorOffsets,
    pub plat: PlatOffsets,
    pub floor: FloorOffsets,
    pub ceiling: CeilingOffsets,
    pub light_flash: LightFlashOffsets,
    pub strobe: StrobeOffsets,
    pub glow: GlowOffsets,
    pub fire_flicker: FireFlickerOffsets,
    pub button: ButtonOffsets,
    pub textline: TextlineOffsets,
    pub stext: StextOffsets,
    /// Strides for the arrays the probe turns pointers into indices against.
    pub state_size: u32,
    pub subsector_size: u32,
}

impl Offsets {
    fn resolve(layout: &Layout) -> Result<Self, ProbeError> {
        Ok(Self {
            thinker: ThinkerOffsets::resolve(layout)?,
            mapthing: MapthingOffsets::resolve(layout)?,
            mobj: MobjOffsets::resolve(layout)?,
            ticcmd: TiccmdOffsets::resolve(layout)?,
            pspdef: PspdefOffsets::resolve(layout)?,
            player: PlayerOffsets::resolve(layout)?,
            sector: SectorOffsets::resolve(layout)?,
            line: LineOffsets::resolve(layout)?,
            side: SideOffsets::resolve(layout)?,
            door: DoorOffsets::resolve(layout)?,
            plat: PlatOffsets::resolve(layout)?,
            floor: FloorOffsets::resolve(layout)?,
            ceiling: CeilingOffsets::resolve(layout)?,
            light_flash: LightFlashOffsets::resolve(layout)?,
            strobe: StrobeOffsets::resolve(layout)?,
            glow: GlowOffsets::resolve(layout)?,
            fire_flicker: FireFlickerOffsets::resolve(layout)?,
            button: ButtonOffsets::resolve(layout)?,
            textline: TextlineOffsets::resolve(layout)?,
            stext: StextOffsets::resolve(layout, layout.size_of("hu_textline_t")?)?,
            state_size: layout.size_of("state_t")?,
            subsector_size: layout.size_of("subsector_t")?,
        })
    }
}

/// A global the probe reads, and how many elements it holds.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Global {
    pub addr: u32,
    pub count: u32,
}

/// Where each engine global lives.
#[derive(Default)]
pub struct Globals {
    pub leveltime: u32,
    pub gametic: u32,
    pub prndindex: u32,
    pub rndindex: u32,
    pub paused: u32,
    pub demoplayback: u32,
    pub totalkills: u32,
    pub totalitems: u32,
    pub totalsecret: u32,
    pub thinkercap: u32,
    pub sectors: u32,
    pub numsectors: u32,
    pub lines: u32,
    pub numlines: u32,
    pub sides: u32,
    pub numsides: u32,
    pub subsectors: u32,
    pub numsubsectors: u32,
    pub states: Global,
    pub texturetranslation: u32,
    pub numtextures: u32,
    pub flattranslation: u32,
    pub numflats: u32,
    pub players: Global,
    pub buttonlist: Global,
    pub activeplats: Global,
    pub activeceilings: Global,
    pub st_faceindex: u32,
    pub st_facecount: u32,
    pub st_priority: u32,
    pub st_lastattackdown: u32,
    pub st_oldweaponsowned: Global,
    pub st_oldhealth: u32,
    pub st_randomnumber: u32,
    pub st_lastcalc: u32,
    pub st_calc_oldhealth: u32,
    pub st_palette: u32,
    pub st_clock: u32,
    pub message_on: u32,
    pub message_counter: u32,
    pub w_message: u32,
    pub message_nottobefuckedwith: u32,
    pub skull_anim_counter: u32,
    pub which_skull: u32,
    pub turnheld: u32,
}

/// Whether `symbol` is `name`, or `name` with the numeric suffix the compiler
/// gives a static declared inside a function.
fn suffixed(symbol: &str, name: &str) -> bool {
    match symbol.strip_prefix(name) {
        Some("") => true,
        Some(tail) => {
            tail.starts_with('.') && tail.len() > 1 && tail[1..].bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// Resolves symbols by name, checking each one is data of the size expected.
struct Names<'a> {
    image: &'a Image,
}

impl Names<'_> {
    /// A named global holding one 32-bit value.
    fn word(&self, name: &str) -> Result<u32, ProbeError> {
        Ok(self.sized(name, 4)?.addr)
    }

    /// A named global holding one 16-bit value.
    fn half(&self, name: &str) -> Result<u32, ProbeError> {
        Ok(self.sized(name, 2)?.addr)
    }

    fn sized(&self, name: &str, size: u32) -> Result<Global, ProbeError> {
        let found = self.find(name)?;
        if found.count != size {
            return Err(ProbeError::SymbolSize {
                name: name.to_owned(),
                want: size,
                got: found.count,
            });
        }
        Ok(Global {
            addr: found.addr,
            count: 1,
        })
    }

    /// An array global, whose element count comes from the symbol's own size.
    fn array(&self, name: &str, element: u32) -> Result<Global, ProbeError> {
        let found = self.find(name)?;
        if element == 0 || !found.count.is_multiple_of(element) {
            return Err(ProbeError::SymbolSize {
                name: name.to_owned(),
                want: element,
                got: found.count,
            });
        }
        Ok(Global {
            addr: found.addr,
            count: found.count / element,
        })
    }

    /// A symbol whose name the compiler may have suffixed, as it does for a
    /// static declared inside a function. Exactly one match, so a second
    /// translation unit declaring the same name is an error rather than a
    /// coin toss.
    fn local(&self, name: &str) -> Result<u32, ProbeError> {
        let mut found =
            self.image.symbols.iter().filter(|symbol| {
                symbol.kind != SymbolKind::Function && suffixed(&symbol.name, name)
            });
        let first = found
            .next()
            .ok_or_else(|| ProbeError::NoSymbol(name.to_owned()))?;
        if found.next().is_some() {
            return Err(ProbeError::AmbiguousSymbol(name.to_owned()));
        }
        if first.size != 4 {
            return Err(ProbeError::SymbolSize {
                name: name.to_owned(),
                want: 4,
                got: first.size,
            });
        }
        Ok(first.addr)
    }

    /// The address and byte size of a data symbol.
    fn find(&self, name: &str) -> Result<Global, ProbeError> {
        let symbol = self
            .image
            .symbol(name)
            .filter(|symbol| symbol.kind != SymbolKind::Function)
            .ok_or_else(|| ProbeError::NoSymbol(name.to_owned()))?;
        Ok(Global {
            addr: symbol.addr,
            count: symbol.size,
        })
    }

    fn function(&self, name: &str) -> Result<u32, ProbeError> {
        self.image
            .symbol(name)
            .filter(|symbol| symbol.kind == SymbolKind::Function)
            .map(|symbol| symbol.addr)
            .ok_or_else(|| ProbeError::NoSymbol(name.to_owned()))
    }
}

/// What a thinker on the list is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ThinkerKind {
    Mobj,
    Door,
    Plat,
    Floor,
    Ceiling,
    LightFlash,
    Strobe,
    Glow,
    FireFlicker,
}

impl ThinkerKind {
    /// The `s_kind` the contract gives this thinker. A mobj has none: it goes
    /// in its own set of columns.
    pub const fn sector_kind(self) -> u8 {
        use clickdoom_spec::native_state::sector_thinker_kind as kind;
        match self {
            ThinkerKind::Mobj => 0,
            ThinkerKind::Door => kind::DOOR,
            ThinkerKind::Plat => kind::PLAT,
            ThinkerKind::Floor => kind::FLOOR,
            ThinkerKind::Ceiling => kind::CEILING,
            ThinkerKind::LightFlash => kind::LIGHT_FLASH,
            ThinkerKind::Strobe => kind::STROBE,
            ThinkerKind::Glow => kind::GLOW,
            ThinkerKind::FireFlicker => kind::FIRE_FLICKER,
        }
    }
}

/// The engine, resolved. Everything a frame dump needs that does not change
/// while the machine runs.
pub struct Engine {
    pub offsets: Offsets,
    pub globals: Globals,
    /// Thinker function address to what it makes the thinker.
    by_function: HashMap<u32, ThinkerKind>,
}

/// The thinker functions the engine recognises, by symbol name.
const THINKER_FUNCTIONS: [(&str, ThinkerKind); 9] = [
    ("P_MobjThinker", ThinkerKind::Mobj),
    ("T_VerticalDoor", ThinkerKind::Door),
    ("T_PlatRaise", ThinkerKind::Plat),
    ("T_MoveFloor", ThinkerKind::Floor),
    ("T_MoveCeiling", ThinkerKind::Ceiling),
    ("T_LightFlash", ThinkerKind::LightFlash),
    ("T_StrobeFlash", ThinkerKind::Strobe),
    ("T_Glow", ThinkerKind::Glow),
    ("T_FireFlicker", ThinkerKind::FireFlicker),
];

impl Engine {
    pub fn resolve(image: &Image, layout: &Layout) -> Result<Self, ProbeError> {
        let names = Names { image };
        let offsets = Offsets::resolve(layout)?;
        let globals = Globals {
            leveltime: names.word("leveltime")?,
            gametic: names.word("gametic")?,
            prndindex: names.word("prndindex")?,
            rndindex: names.word("rndindex")?,
            paused: names.word("paused")?,
            demoplayback: names.word("demoplayback")?,
            totalkills: names.word("totalkills")?,
            totalitems: names.word("totalitems")?,
            totalsecret: names.word("totalsecret")?,
            thinkercap: names.sized("thinkercap", offsets.thinker.size)?.addr,
            sectors: names.word("sectors")?,
            numsectors: names.word("numsectors")?,
            lines: names.word("lines")?,
            numlines: names.word("numlines")?,
            sides: names.word("sides")?,
            numsides: names.word("numsides")?,
            subsectors: names.word("subsectors")?,
            numsubsectors: names.word("numsubsectors")?,
            states: names.array("states", layout.size_of("state_t")?)?,
            texturetranslation: names.word("texturetranslation")?,
            numtextures: names.word("numtextures")?,
            flattranslation: names.word("flattranslation")?,
            numflats: names.word("numflats")?,
            players: names.array("players", offsets.player.size)?,
            buttonlist: names.array("buttonlist", offsets.button.size)?,
            activeplats: names.array("activeplats", 4)?,
            activeceilings: names.array("activeceilings", 4)?,
            st_faceindex: names.word("st_faceindex")?,
            st_facecount: names.word("st_facecount")?,
            st_priority: names.local("priority")?,
            st_lastattackdown: names.local("lastattackdown")?,
            st_oldweaponsowned: names.array("oldweaponsowned", 4)?,
            st_oldhealth: names.word("st_oldhealth")?,
            st_randomnumber: names.word("st_randomnumber")?,
            st_lastcalc: names.local("lastcalc")?,
            st_calc_oldhealth: names.local("oldhealth")?,
            st_palette: names.word("st_palette")?,
            st_clock: names.word("st_clock")?,
            message_on: names.word("message_on")?,
            message_counter: names.word("message_counter")?,
            w_message: names.sized("w_message", offsets.stext.size)?.addr,
            message_nottobefuckedwith: names.word("message_nottobefuckedwith")?,
            skull_anim_counter: names.half("skullAnimCounter")?,
            which_skull: names.half("whichSkull")?,
            turnheld: names.word("turnheld")?,
        };

        let mut by_function = HashMap::new();
        for (name, kind) in THINKER_FUNCTIONS {
            by_function.insert(names.function(name)?, kind);
        }

        Ok(Self {
            offsets,
            globals,
            by_function,
        })
    }
}

/// The engine's tic counter as it stands.
pub fn gametic(engine: &Engine, cpu: &Cpu) -> Result<i32, ProbeError> {
    ram_of(cpu).i32(engine.globals.gametic, "gametic")
}

/// The machine's RAM, addressed the way the program addresses it.
pub fn ram_of(cpu: &Cpu) -> Ram<'_> {
    Ram::new(cpu.memory.map().ram_base, cpu.memory.ram())
}

/// One entry of the thinker list, in list order.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Thinker {
    pub addr: u32,
    pub kind: ThinkerKind,
    /// Whether the thinker's function runs. A plat or a ceiling put in stasis
    /// stays on the list with no function.
    pub active: bool,
}

/// The thinker list as it stands, split into mobjs and sector thinkers.
///
/// Slots are one-based positions within each of the two sequences, which is
/// what a pointer between thinkers is written as.
#[derive(PartialEq, Eq, Debug)]
pub struct Walk {
    pub mobjs: Vec<Thinker>,
    pub sector_thinkers: Vec<Thinker>,
    slots: HashMap<u32, u32>,
    sector_slots: HashMap<u32, u32>,
}

impl Walk {
    /// The one-based slot of the mobj at this address, 0 for none.
    pub fn mobj_slot(&self, addr: u32) -> u32 {
        self.slots.get(&addr).copied().unwrap_or(0)
    }

    /// The one-based slot of the sector thinker at this address, 0 for none.
    pub fn sector_slot(&self, addr: u32) -> u32 {
        self.sector_slots.get(&addr).copied().unwrap_or(0)
    }
}

/// Walks the thinker list once, in the order the engine runs it.
///
/// A removed thinker is skipped: it is waiting to be unlinked and the SQL side
/// has already dropped it. A thinker whose function names nothing known is an
/// error, because dropping it silently would make the two sides disagree for a
/// reason nobody could see.
pub fn walk(engine: &Engine, cpu: &Cpu) -> Result<Walk, ProbeError> {
    let ram = ram_of(cpu);
    let offsets = &engine.offsets.thinker;
    let cap = engine.globals.thinkercap;

    let in_stasis = stasis_kinds(engine, &ram)?;
    let mut walk = Walk {
        mobjs: Vec::new(),
        sector_thinkers: Vec::new(),
        slots: HashMap::new(),
        sector_slots: HashMap::new(),
    };

    let mut at = ram.u32(cap + offsets.next, "thinkercap.next")?;
    let mut steps = 0u32;
    while at != cap {
        steps += 1;
        if steps > THINKER_LIMIT {
            return Err(ProbeError::ThinkerListRuns(THINKER_LIMIT));
        }
        let function = ram.u32(at + offsets.function, "a thinker's function")?;
        let (kind, active) = match function {
            THINKER_REMOVED => {
                at = ram.u32(at + offsets.next, "a thinker's next link")?;
                continue;
            }
            THINKER_STASIS => (
                *in_stasis
                    .get(&at)
                    .ok_or(ProbeError::UnknownThinker { addr: at, function })?,
                false,
            ),
            _ => (
                *engine
                    .by_function
                    .get(&function)
                    .ok_or(ProbeError::UnknownThinker { addr: at, function })?,
                true,
            ),
        };
        let thinker = Thinker {
            addr: at,
            kind,
            active,
        };
        if kind == ThinkerKind::Mobj {
            walk.mobjs.push(thinker);
            walk.slots.insert(at, walk.mobjs.len() as u32);
        } else {
            walk.sector_thinkers.push(thinker);
            walk.sector_slots
                .insert(at, walk.sector_thinkers.len() as u32);
        }
        at = ram.u32(at + offsets.next, "a thinker's next link")?;
    }
    Ok(walk)
}

/// What a thinker with no function is, taken from the tables the engine keeps
/// of the ones it can stop: plats and ceilings.
fn stasis_kinds(engine: &Engine, ram: &Ram<'_>) -> Result<HashMap<u32, ThinkerKind>, ProbeError> {
    let mut kinds = HashMap::new();
    for (table, kind, what) in [
        (engine.globals.activeplats, ThinkerKind::Plat, "activeplats"),
        (
            engine.globals.activeceilings,
            ThinkerKind::Ceiling,
            "activeceilings",
        ),
    ] {
        for index in 0..table.count {
            let entry = ram.u32(table.addr + index * 4, what)?;
            if entry != 0 {
                kinds.insert(entry, kind);
            }
        }
    }
    Ok(kinds)
}

/// The zero-based index of `addr` in the array of `stride`-byte elements at
/// `base`, or an error when it is not one of them.
pub fn index_of(
    base: u32,
    count: u32,
    stride: u32,
    addr: u32,
    what: &'static str,
    into: &'static str,
) -> Result<i32, ProbeError> {
    let offset = addr.wrapping_sub(base);
    if addr < base || !offset.is_multiple_of(stride) || offset / stride >= count {
        return Err(ProbeError::NotAnIndex {
            what,
            addr: base,
            value: addr,
            into,
        });
    }
    Ok((offset / stride) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::layout::Layout;
    use clickdoom_spec::RAM_BASE;

    /// Enough of the table for a thinker walk. The offsets are the pinned
    /// ROM's, so the fixture and the real thing agree on the shape.
    const TABLE: &str = "\
thinker_t\tsizeof\t0\t12
thinker_t\tprev\t0\t4
thinker_t\tnext\t4\t4
thinker_t\tfunction\t8\t4
";

    const CAP: u32 = RAM_BASE + 0x1000;
    const PLATS: u32 = RAM_BASE + 0x2000;
    const CEILINGS: u32 = RAM_BASE + 0x2100;
    const MOBJ_THINKER: u32 = 0x8000_1234;
    const DOOR_THINKER: u32 = 0x8000_5678;
    const PLAT_THINKER: u32 = 0x8000_9ABC;

    /// A machine with a thinker list of `(address, function)` pairs, linked in
    /// the order given and closed back onto the list head.
    fn machine(entries: &[(u32, u32)]) -> Cpu {
        fn link(cpu: &mut Cpu, from: u32, to: u32) {
            cpu.memory.write(from + 4, 4, to, 0).unwrap();
            cpu.memory.write(to, 4, from, 0).unwrap();
        }
        let mut cpu = Cpu::inert();
        let mut previous = CAP;
        for (addr, function) in entries {
            link(&mut cpu, previous, *addr);
            cpu.memory.write(addr + 8, 4, *function, 0).unwrap();
            previous = *addr;
        }
        link(&mut cpu, previous, CAP);
        cpu
    }

    fn engine(plats: &[u32], ceilings: &[u32]) -> Engine {
        let layout = Layout::parse(TABLE).unwrap();
        let mut by_function = HashMap::new();
        by_function.insert(MOBJ_THINKER, ThinkerKind::Mobj);
        by_function.insert(DOOR_THINKER, ThinkerKind::Door);
        by_function.insert(PLAT_THINKER, ThinkerKind::Plat);
        Engine {
            offsets: Offsets {
                thinker: ThinkerOffsets::resolve(&layout).unwrap(),
                ..offsets_of(&layout)
            },
            globals: Globals {
                thinkercap: CAP,
                activeplats: Global {
                    addr: PLATS,
                    count: plats.len() as u32,
                },
                activeceilings: Global {
                    addr: CEILINGS,
                    count: ceilings.len() as u32,
                },
                ..Globals::default()
            },
            by_function,
        }
    }

    /// The rest of the offsets, which a walk does not read. Resolved from the
    /// committed table so the fixture stays one table rather than two.
    fn offsets_of(_layout: &Layout) -> Offsets {
        let full = Layout::parse(include_str!("../../probe/layout.tsv")).unwrap();
        Offsets::resolve(&full).unwrap()
    }

    /// Fills the engine's active tables with the addresses given.
    fn fill(cpu: &mut Cpu, base: u32, entries: &[u32]) {
        for (index, entry) in entries.iter().enumerate() {
            cpu.memory
                .write(base + index as u32 * 4, 4, *entry, 0)
                .unwrap();
        }
    }

    #[test]
    fn a_two_entry_thinker_list_yields_two_mobjs_in_list_order() {
        let first = RAM_BASE + 0x3000;
        let second = RAM_BASE + 0x3100;
        let cpu = machine(&[(first, MOBJ_THINKER), (second, MOBJ_THINKER)]);
        let engine = engine(&[], &[]);
        let walk = walk(&engine, &cpu).unwrap();
        assert_eq!(walk.mobjs.len(), 2);
        assert_eq!(walk.mobjs[0].addr, first);
        assert_eq!(walk.mobjs[1].addr, second);
        assert_eq!(walk.mobj_slot(first), 1);
        assert_eq!(walk.mobj_slot(second), 2);
        assert_eq!(
            walk.mobj_slot(RAM_BASE),
            0,
            "an unlisted address has no slot"
        );
        assert!(walk.sector_thinkers.is_empty());
        assert!(walk.mobjs.iter().all(|t| t.active));
    }

    #[test]
    fn a_removed_thinker_is_skipped_and_does_not_take_a_slot() {
        let first = RAM_BASE + 0x3000;
        let dead = RAM_BASE + 0x3100;
        let second = RAM_BASE + 0x3200;
        let cpu = machine(&[
            (first, MOBJ_THINKER),
            (dead, THINKER_REMOVED),
            (second, MOBJ_THINKER),
        ]);
        let walk = walk(&engine(&[], &[]), &cpu).unwrap();
        assert_eq!(walk.mobjs.len(), 2);
        assert_eq!(walk.mobj_slot(dead), 0);
        assert_eq!(
            walk.mobj_slot(second),
            2,
            "the slot after it does not shift"
        );
    }

    #[test]
    fn a_mobj_and_a_sector_thinker_are_numbered_in_separate_sequences() {
        let mobj = RAM_BASE + 0x3000;
        let door = RAM_BASE + 0x3100;
        let mobj2 = RAM_BASE + 0x3200;
        let cpu = machine(&[
            (mobj, MOBJ_THINKER),
            (door, DOOR_THINKER),
            (mobj2, MOBJ_THINKER),
        ]);
        let walk = walk(&engine(&[], &[]), &cpu).unwrap();
        assert_eq!(walk.mobj_slot(mobj), 1);
        assert_eq!(walk.mobj_slot(mobj2), 2);
        assert_eq!(walk.sector_slot(door), 1);
        assert_eq!(walk.sector_thinkers[0].kind, ThinkerKind::Door);
    }

    #[test]
    fn a_thinker_in_stasis_is_named_by_the_table_it_is_still_in() {
        let plat = RAM_BASE + 0x3000;
        let mut cpu = machine(&[(plat, THINKER_STASIS)]);
        fill(&mut cpu, PLATS, &[0, plat]);
        let walk = walk(&engine(&[0, plat], &[]), &cpu).unwrap();
        assert_eq!(walk.sector_thinkers.len(), 1);
        assert_eq!(walk.sector_thinkers[0].kind, ThinkerKind::Plat);
        assert!(!walk.sector_thinkers[0].active, "stasis is not running");
    }

    #[test]
    fn a_thinker_nothing_names_is_an_error_rather_than_a_dropped_row() {
        let unknown = RAM_BASE + 0x3000;
        let cpu = machine(&[(unknown, 0x8000_DEAD)]);
        assert_eq!(
            walk(&engine(&[], &[]), &cpu),
            Err(ProbeError::UnknownThinker {
                addr: unknown,
                function: 0x8000_DEAD,
            })
        );
        // A thinker with no function that is in neither active table is the
        // same kind of unknown.
        let cpu = machine(&[(unknown, THINKER_STASIS)]);
        assert!(matches!(
            walk(&engine(&[], &[]), &cpu),
            Err(ProbeError::UnknownThinker { .. })
        ));
    }

    #[test]
    fn a_list_that_never_returns_to_its_head_is_an_error() {
        let looped = RAM_BASE + 0x3000;
        let mut cpu = machine(&[(looped, THINKER_REMOVED)]);
        // Point the entry at itself, so the walk never reaches the head.
        cpu.memory.write(looped + 4, 4, looped, 0).unwrap();
        assert_eq!(
            walk(&engine(&[], &[]), &cpu),
            Err(ProbeError::ThinkerListRuns(THINKER_LIMIT))
        );
    }

    #[test]
    fn a_link_pointing_outside_ram_is_an_error_naming_the_address() {
        let mut cpu = machine(&[]);
        cpu.memory.write(CAP + 4, 4, 0x1000_0000, 0).unwrap();
        assert!(matches!(
            walk(&engine(&[], &[]), &cpu),
            Err(ProbeError::OutsideRam { .. })
        ));
    }

    #[test]
    fn a_static_the_compiler_suffixed_still_matches_its_name() {
        assert!(suffixed("priority", "priority"));
        assert!(suffixed("priority.2", "priority"));
        assert!(suffixed("oldhealth.14", "oldhealth"));
        assert!(!suffixed("priority.x", "priority"));
        assert!(!suffixed("priority.", "priority"));
        assert!(!suffixed("priority_2", "priority"));
        assert!(!suffixed("st_priority", "priority"));
    }

    #[test]
    fn every_sector_thinker_kind_has_a_contract_value_of_its_own() {
        let mut seen = std::collections::HashSet::new();
        for (_, kind) in THINKER_FUNCTIONS {
            if kind == ThinkerKind::Mobj {
                continue;
            }
            assert!(seen.insert(kind.sector_kind()), "{kind:?} repeats a value");
            assert_ne!(kind.sector_kind(), 0, "{kind:?} has no value");
        }
    }
}
