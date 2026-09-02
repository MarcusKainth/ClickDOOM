//! One thing moving through the loaded level, as a statement.
//!
//! `mobj::xy_movement` reads a world of arrays and answers with the
//! accumulator its fold ended on. The tic statement hands it the state
//! row; this hands it a world with one thing in it, so a test can put a
//! mover anywhere and give it any momentum. The map is the real one, so
//! what the move meets is the level's own geometry.

use clickdoom_native::sql::bsp;
use clickdoom_native::sql::sim::{self, inter, map, mobj};

/// Something else standing in the level, solid and in the way.
pub struct Blocker {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub radius: i32,
    pub height: i32,
}

/// The thing being moved, in map units already scaled to fixed point.
pub struct Mover {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub momx: i32,
    pub momy: i32,
    pub radius: i32,
    pub height: i32,
    /// What the thing faces, which is what `P_UseLines` reaches along.
    pub angle: u32,
    /// 1 to reach for a line before moving.
    pub uses: u8,
}

impl Mover {
    /// A thing the size of the player, standing still at `(x, y)`.
    pub fn player_sized(x: i32, y: i32) -> Mover {
        Mover {
            x,
            y,
            z: 0,
            momx: 0,
            momy: 0,
            radius: 16 << 16,
            height: 56 << 16,
            angle: 0,
            uses: 0,
        }
    }
}

/// What the fold left, in the order [`select`] asks for it.
#[derive(clickhouse::Row, serde::Deserialize, Debug, PartialEq, Eq)]
pub struct Moved {
    pub x: i32,
    pub y: i32,
    pub xmove: i64,
    pub ymove: i64,
    pub phase: i64,
}

/// The statement that moves `mover` one tic through the level in `db`.
///
/// The level's own state row holds the sectors and lines, so the caller
/// has to have run the setup statements. The mobj arrays are the mover
/// alone, which is what keeps the answer about the map rather than about
/// whatever else the level spawned.
pub fn select(db: &str, mover: &Mover) -> String {
    with_blockers(db, mover, &[])
}

/// [`select`] with other things on the blockmap for the move to meet.
pub fn with_blockers(db: &str, mover: &Mover, blockers: &[Blocker]) -> String {
    body(
        db,
        mover,
        blockers,
        &mover.x.to_string(),
        &mover.y.to_string(),
        &mover.momx.to_string(),
        &mover.momy.to_string(),
        "SELECT 1",
    )
}

/// One statement over many movers, each a `(x, y, momx, momy)` tuple in
/// `cases`, which is how a search for a geometry stays one query.
pub fn sweep(db: &str, mover: &Mover, cases: &str) -> String {
    body(
        db,
        mover,
        &[],
        "c.1",
        "c.2",
        "c.3",
        "c.4",
        &format!("SELECT arrayJoin({cases}) AS c"),
    )
}

#[allow(clippy::too_many_arguments)]
fn body(
    db: &str,
    mover: &Mover,
    blockers: &[Blocker],
    mx: &str,
    my: &str,
    mmomx: &str,
    mmomy: &str,
    source: &str,
) -> String {
    let held = |field: usize| format!("moved.{field}");
    let at_zero = |column: &str| format!("joinGet('{db}.native_state', '{column}', toUInt32(0))");
    let one = |cast: &str, value: String| format!("CAST([{value}], 'Array({cast})')");

    let flags = MF_SOLID.to_string();
    let list = |first: String, rest: Box<dyn Fn(&Blocker) -> String>| {
        let mut all = vec![first];
        all.extend(blockers.iter().map(&rest));
        all.join(", ")
    };
    let m_x = one("Int32", list(mx.to_owned(), Box::new(|b| b.x.to_string())));
    let m_y = one("Int32", list(my.to_owned(), Box::new(|b| b.y.to_string())));
    let m_radius = one(
        "Int32",
        list(mover.radius.to_string(), Box::new(|b| b.radius.to_string())),
    );
    let m_flags = one(
        "Int32",
        list(flags.clone(), Box::new(|_| MF_SOLID.to_string())),
    );
    let m_linkseq = one("UInt32", list("1".to_owned(), Box::new(|_| "1".to_owned())));
    let m_sprite = one(
        "Int32",
        list("-1".to_owned(), Box::new(|_| "-1".to_owned())),
    );
    let m_z = one(
        "Int32",
        list(mover.z.to_string(), Box::new(|b| b.z.to_string())),
    );
    let alive = format!("move_at.{}", mobj::moving::ALIVE);

    let world = map::World {
        m_x: &m_x,
        m_y: &m_y,
        m_radius: &m_radius,
        m_flags: &m_flags,
        m_linkseq: &m_linkseq,
        alive: &alive,
        floorheight: &at_zero("sec_floorheight"),
        ceilingheight: &at_zero("sec_ceilingheight"),
        line_special: &at_zero("line_special"),
    };
    let no_player = inter::Player {
        health: "100",
        armorpoints: "0",
        armortype: "0",
        ammo: "CAST([0, 0, 0, 0], 'Array(Int32)')",
        maxammo: "CAST([200, 50, 300, 50], 'Array(Int32)')",
        backpack: "0",
        cards: "CAST([0, 0, 0, 0, 0, 0], 'Array(UInt8)')",
        powers: "CAST([0, 0, 0, 0, 0, 0], 'Array(Int32)')",
        weaponowned: "CAST([1, 1, 0, 0, 0, 0, 0, 0, 0], 'Array(Int32)')",
        pendingweapon: "-1",
        message: "0",
        itemcount: "0",
        bonuscount: "0",
        mo_flags: &flags,
    };
    let start = inter::start(&no_player);
    let pickups = mobj::Pickups {
        m_sprite: &m_sprite,
        m_flags: &m_flags,
        m_z: &m_z,
        skill: "2",
        start: &start,
        alive: &one("UInt8", list("1".to_owned(), Box::new(|_| "1".to_owned()))),
    };
    let momx = mmomx.to_owned();
    let momy = mmomy.to_owned();
    let angle = mover.angle.to_string();
    let uses = mover.uses.to_string();
    let z = mover.z.to_string();
    let height = mover.height.to_string();
    let radius = mover.radius.to_string();
    let x = mx.to_owned();
    let y = my.to_owned();
    let nodes = bsp::Nodes {
        x: "node_x",
        y: "node_y",
        dx: "node_dx",
        dy: "node_dy",
        child0: "node_child0",
        child1: "node_child1",
        count: "numnodes",
    };
    let subsector = format!(
        "toInt32({})",
        bsp::point_in_subsector(&x, &y, &nodes, "bsp_depth")
    );
    let sector = format!("1 + ssec_sector[1 + {subsector}]");
    let floorz = format!("{}[{sector}]", at_zero("sec_floorheight"));
    let ceilingz = format!("{}[{sector}]", at_zero("sec_ceilingheight"));
    let moving = mobj::Mover {
        slot: "1",
        radius: &radius,
        height: &height,
        z: &z,
        flags: &flags,
        is_player: "1",
        momx: &momx,
        momy: &momy,
        x: &x,
        y: &y,
        floorz: &floorz,
        ceilingz: &ceilingz,
        subsector: &subsector,
        angle: &angle,
        uses: &uses,
    };
    let fold = mobj::xy_movement(&moving, &world, &pickups);
    format!(
        "SELECT\n    toInt32({}) AS x,\n    toInt32({}) AS y,\n    \
         toInt64({}) AS xmove,\n    toInt64({}) AS ymove,\n    \
         toInt64({}) AS phase\nFROM\n(\n    WITH\n{}\n    \
         {source}, ({fold}) AS moved\n)",
        held(mobj::moving::X),
        held(mobj::moving::Y),
        held(mobj::moving::XMOVE),
        held(mobj::moving::YMOVE),
        held(mobj::moving::PHASE),
        sim::constants(db)
            .into_iter()
            // `P_TouchSpecialThing` reads the weapon the player holds, which
            // the tic statement binds in the stage above the move.
            .chain([("pk_readyweapon".to_owned(), "toInt64(1)".to_owned())])
            .map(|(name, expr)| format!("        ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
    )
}

/// `p_mobj.h`
const MF_SOLID: i64 = 2;
