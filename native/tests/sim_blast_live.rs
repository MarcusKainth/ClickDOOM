//! `A_Explode` and `P_RadiusAttack` against a real ClickHouse server.
//!
//! The world is the one `P_SetupLevel` leaves on `E1M7`, so the block walk,
//! the order inside a cell and the line of sight are the level's own. Every
//! ask is compared against `native/tests/support/attacks.rs`, a reader
//! written from `p_map.c`, which works the walk out from the blockmap
//! header rather than from anything the statement says.
//!
//! The sight answers come from `P_CheckSight` in a query of their own.
//! `sim_sight_live` is what checks that routine against its own reader;
//! what this checks is what `PIT_RadiusAttack` does with the answer.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, attacks, inter, sight};
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::attacks::{Blockmap, Bomb, Standing};
use support::damage::{Mobj, World as Damage};
use support::db::Fixture;
use support::mobj::thing_type;

/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;

/// The random indices the fan blasts from, and the draw offsets it blasts
/// at.
const INDICES: [i64; 3] = [0, 61, 137];
const BASES: [i64; 2] = [0, 17];

#[tokio::test]
async fn a_blast_hurts_what_the_engine_hurts() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_blast").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let map = blockmap(&fixture, &db).await;
    let (mut mobjs, standing, subsector) = world(&fixture, &db).await;
    let spots = spots(&mobjs, &standing, &map);
    assert!(spots.len() > 2, "the level holds spots worth blasting from");

    // Two of the things the first spot reaches become the bosses
    // concussion does not touch, so the fan reaches that arm on a map that
    // carries neither.
    let bosses = boss_slots(&mobjs, &standing, &map, spots[0]);
    for (at, slot) in bosses.iter().enumerate() {
        mobjs[slot - 1].kind = thing_type(["MT_CYBORG", "MT_SPIDER"][at]);
    }
    assert_eq!(bosses.len(), 2, "the first spot reaches two things to swap");

    let mut coverage = Coverage::default();
    for prnd in INDICES {
        for base in BASES {
            let hurt = Damage {
                mobjs: mobjs.clone(),
                prndindex: prnd,
                readyweapon: 0,
            };
            for spot in &spots {
                let source = mobjs[spot - 1].target as usize;
                let candidates =
                    support::attacks::radius_candidates(&mobjs, &standing, &map, *spot);
                let seen = seen(&fixture, &db, &subsector, &mobjs, *spot, &candidates).await;
                let blasting = support::attacks::Blasting {
                    mobjs: &mobjs,
                    standing: &standing,
                    hurt: &hurt,
                };
                let (want, drew) = blasting.asks(*spot, source, base, &candidates, &|slot| {
                    seen[candidates.iter().position(|held| *held == slot).unwrap()]
                });
                let (ours, drawn) = ask_server(
                    &fixture, &db, &mobjs, &standing, &subsector, *spot, prnd, base,
                )
                .await;
                assert_eq!(ours, want, "index {prnd}, base {base}, spot {spot}");
                assert_eq!(drawn, drew, "index {prnd}, base {base}, spot {spot} draws");
                coverage.count(&mobjs, &standing, *spot, &candidates, &seen);
            }
        }
    }
    fixture.finish().await;
    coverage.check();
}

/// That the fan reached every arm `PIT_RadiusAttack` has.
#[derive(Default)]
struct Coverage {
    seen: usize,
    unseen: usize,
    far: usize,
    unshootable: usize,
    boss: usize,
    stacked: usize,
}

impl Coverage {
    fn count(
        &mut self,
        mobjs: &[Mobj],
        standing: &[Standing],
        spot: usize,
        candidates: &[usize],
        seen: &[bool],
    ) {
        self.seen += seen.iter().filter(|v| **v).count();
        self.unseen += seen.iter().filter(|v| !**v).count();
        if seen.iter().filter(|v| **v).count() > 1 {
            self.stacked += 1;
        }
        let cyborg = thing_type("MT_CYBORG");
        let spider = thing_type("MT_SPIDER");
        for slot in 1..=mobjs.len() {
            if candidates.contains(&slot) || slot == spot {
                continue;
            }
            let it = &mobjs[slot - 1];
            if support::attacks::blast_damage(mobjs, standing, spot, slot) <= 0 {
                self.far += 1;
            } else if it.flags & MF_SHOOTABLE == 0 {
                self.unshootable += 1;
            } else if it.kind == cyborg || it.kind == spider {
                self.boss += 1;
            }
        }
    }

    fn check(&self) {
        assert!(
            self.seen > 0
                && self.unseen > 0
                && self.far > 0
                && self.unshootable > 0
                && self.boss > 0
                && self.stacked > 0,
            "the fan reaches every arm: seen {}, unseen {}, far {}, unshootable {}, \
             boss {}, blasts hurting more than one {}",
            self.seen,
            self.unseen,
            self.far,
            self.unshootable,
            self.boss,
            self.stacked
        );
    }
}

/// The blockmap's own header, which is what the walk indexes into.
async fn blockmap(fixture: &Fixture, db: &str) -> Blockmap {
    #[derive(Row, Deserialize)]
    struct Header {
        orgx: i32,
        orgy: i32,
        cols: i32,
        rows: i32,
    }
    let row: Header = fixture
        .scalar(&format!(
            "SELECT toInt32(origin_x) AS orgx, toInt32(origin_y) AS orgy, \
             toInt32(columns) AS cols, toInt32(rows) AS rows \
             FROM {db}.lv_blockmap_header LIMIT 1"
        ))
        .await;
    Blockmap {
        orgx: i64::from(row.orgx),
        orgy: i64::from(row.orgy),
        cols: i64::from(row.cols),
        rows: i64::from(row.rows),
    }
}

/// The things `P_SetupLevel` left, on the fields a blast and a damage call
/// read.
async fn world(fixture: &Fixture, db: &str) -> (Vec<Mobj>, Vec<Standing>, Vec<i64>) {
    #[derive(Row, Deserialize)]
    struct Arrays {
        x: Vec<i32>,
        y: Vec<i32>,
        z: Vec<i32>,
        kind: Vec<i32>,
        state: Vec<i32>,
        tics: Vec<i32>,
        flags: Vec<i32>,
        health: Vec<i32>,
        height: Vec<i32>,
        radius: Vec<i32>,
        target: Vec<u32>,
        threshold: Vec<i32>,
        player: Vec<i8>,
        linkseq: Vec<u32>,
        subsector: Vec<i32>,
    }
    let row: Arrays = fixture
        .scalar(&format!(
            "SELECT m_x AS x, m_y AS y, m_z AS z, m_type AS kind, m_state AS state, \
             m_tics AS tics, m_flags AS flags, m_health AS health, m_height AS height, \
             m_radius AS radius, m_target AS target, m_threshold AS threshold, \
             m_player AS player, m_linkseq AS linkseq, m_subsector AS subsector \
             FROM {db}.native_state WHERE tic = 0"
        ))
        .await;
    let mobjs = (0..row.x.len())
        .map(|at| Mobj {
            x: i64::from(row.x[at]),
            y: i64::from(row.y[at]),
            z: i64::from(row.z[at]),
            momx: 0,
            momy: 0,
            momz: 0,
            kind: i64::from(row.kind[at]),
            state: i64::from(row.state[at]),
            tics: i64::from(row.tics[at]),
            flags: i64::from(row.flags[at]),
            health: i64::from(row.health[at]),
            height: i64::from(row.height[at]),
            target: i64::from(row.target[at]),
            threshold: i64::from(row.threshold[at]),
            player: i64::from(row.player[at]),
        })
        .collect();
    let standing = (0..row.x.len())
        .map(|at| Standing {
            radius: i64::from(row.radius[at]),
            linkseq: i64::from(row.linkseq[at]),
        })
        .collect();
    let subsector = row.subsector.iter().map(|v| i64::from(*v)).collect();
    (mobjs, standing, subsector)
}

/// The slots worth blasting from: the first few that reach more than one
/// thing, spread far enough apart to cover different geometry.
fn spots(mobjs: &[Mobj], standing: &[Standing], map: &Blockmap) -> Vec<usize> {
    let mut spots: Vec<usize> = Vec::new();
    for slot in 1..=mobjs.len() {
        if support::attacks::radius_candidates(mobjs, standing, map, slot).len() < 2 {
            continue;
        }
        if spots.iter().any(|held| {
            (mobjs[held - 1].x - mobjs[slot - 1].x).abs()
                + (mobjs[held - 1].y - mobjs[slot - 1].y).abs()
                < 512 * 65536
        }) {
            continue;
        }
        spots.push(slot);
        if spots.len() == 4 {
            break;
        }
    }
    spots
}

/// Two of the things a spot reaches, to stand in for the bosses.
fn boss_slots(mobjs: &[Mobj], standing: &[Standing], map: &Blockmap, spot: usize) -> Vec<usize> {
    support::attacks::radius_candidates(mobjs, standing, map, spot)
        .into_iter()
        .take(2)
        .collect()
}

fn literal(of: &[i64]) -> String {
    format!(
        "[{}]",
        of.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
    )
}

fn constants(db: &str) -> String {
    sim::constants(db)
        .into_iter()
        .map(|(name, expr)| format!("    ({expr}) AS {name}"))
        .collect::<Vec<_>>()
        .join(",\n")
}

/// The array literals both queries read the world through.
struct Arrays {
    m_x: String,
    m_y: String,
    m_z: String,
    m_radius: String,
    m_height: String,
    m_flags: String,
    m_type: String,
    m_subsector: String,
    m_linkseq: String,
    alive: String,
    m_state: String,
    m_tics: String,
    m_health: String,
    m_target: String,
    m_threshold: String,
    m_player: String,
    zero: String,
}

impl Arrays {
    fn of(mobjs: &[Mobj], standing: &[Standing], subsector: &[i64]) -> Arrays {
        let of =
            |get: &dyn Fn(usize) -> i64| literal(&(0..mobjs.len()).map(get).collect::<Vec<_>>());
        Arrays {
            m_x: of(&|at| mobjs[at].x),
            m_y: of(&|at| mobjs[at].y),
            m_z: of(&|at| mobjs[at].z),
            m_radius: of(&|at| standing[at].radius),
            m_height: of(&|at| mobjs[at].height),
            m_flags: of(&|at| mobjs[at].flags),
            m_type: of(&|at| mobjs[at].kind),
            m_subsector: of(&|at| subsector[at]),
            m_linkseq: of(&|at| standing[at].linkseq),
            alive: format!("CAST({}, 'Array(UInt8)')", of(&|_| 1)),
            m_state: of(&|at| mobjs[at].state),
            m_tics: of(&|at| mobjs[at].tics),
            m_health: of(&|at| mobjs[at].health),
            m_target: format!("CAST({}, 'Array(UInt32)')", of(&|at| mobjs[at].target)),
            m_threshold: of(&|at| mobjs[at].threshold),
            m_player: of(&|at| mobjs[at].player),
            zero: of(&|_| 0),
        }
    }

    fn blast(&self) -> attacks::Blast<'_> {
        attacks::Blast {
            m_x: &self.m_x,
            m_y: &self.m_y,
            m_z: &self.m_z,
            m_radius: &self.m_radius,
            m_height: &self.m_height,
            m_flags: &self.m_flags,
            m_type: &self.m_type,
            m_subsector: &self.m_subsector,
            m_linkseq: &self.m_linkseq,
            alive: &self.alive,
        }
    }

    fn hurting<'a>(&'a self, prndindex: &'a str) -> inter::Hurting<'a> {
        inter::Hurting {
            m_x: &self.m_x,
            m_y: &self.m_y,
            m_z: &self.m_z,
            m_momx: &self.zero,
            m_momy: &self.zero,
            m_momz: &self.zero,
            m_reactiontime: &self.zero,
            m_type: &self.m_type,
            m_state: &self.m_state,
            m_tics: &self.m_tics,
            m_flags: &self.m_flags,
            m_health: &self.m_health,
            m_height: &self.m_height,
            m_target: &self.m_target,
            m_threshold: &self.m_threshold,
            m_player: &self.m_player,
            prndindex,
            readyweapon: "0",
        }
    }
}

/// `P_CheckSight (thing, bombspot)` for each candidate, in the walk's own
/// order.
async fn seen(
    fixture: &Fixture,
    db: &str,
    subsector: &[i64],
    mobjs: &[Mobj],
    spot: usize,
    candidates: &[usize],
) -> Vec<bool> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let of = |slot: usize, get: &dyn Fn(usize) -> i64| get(slot - 1).to_string();
    let pairs: Vec<String> = candidates
        .iter()
        .map(|slot| {
            sight::asking(
                &of(*slot, &|at| subsector[at]),
                &of(*slot, &|at| mobjs[at].x),
                &of(*slot, &|at| mobjs[at].y),
                &of(*slot, &|at| mobjs[at].z),
                &of(*slot, &|at| mobjs[at].height),
                &of(spot, &|at| subsector[at]),
                &of(spot, &|at| mobjs[at].x),
                &of(spot, &|at| mobjs[at].y),
                &of(spot, &|at| mobjs[at].z),
                &of(spot, &|at| mobjs[at].height),
            )
        })
        .collect();
    #[derive(Row, Deserialize)]
    struct Seen {
        seen: Vec<u8>,
    }
    let heights = sight::seg_openings(&sight::Heights {
        floorheight: &format!("joinGet('{db}.native_state', 'sec_floorheight', toUInt32(0))"),
        ceilingheight: &format!("joinGet('{db}.native_state', 'sec_ceilingheight', toUInt32(0))"),
    });
    let sql = format!(
        "WITH\n{},\n{}\nSELECT {} AS seen",
        constants(db),
        heights
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        sight::check_sight(&format!("[{}]", pairs.join(", "))),
    );
    let ours: Seen = fixture.scalar(&sql).await;
    ours.seen.into_iter().map(|v| v == 1).collect()
}

/// The blast's own answer: the damage asks it makes and what they draw.
#[allow(clippy::too_many_arguments)]
async fn ask_server(
    fixture: &Fixture,
    db: &str,
    mobjs: &[Mobj],
    standing: &[Standing],
    subsector: &[i64],
    spot: usize,
    prnd: i64,
    base: i64,
) -> (Vec<Bomb>, i64) {
    let arrays = Arrays::of(mobjs, standing, subsector);
    let prndindex = prnd.to_string();
    let hurting = arrays.hurting(&prndindex);
    let source = mobjs[spot - 1].target;
    let list = format!("[(toUInt32({spot}), toUInt32({source}), toUInt32({base}))]");
    #[derive(Row, Deserialize)]
    struct Bombed {
        asks: Vec<(u32, u32, u32, i32, u32)>,
        draws: u32,
    }
    let heights = sight::seg_openings(&sight::Heights {
        floorheight: &format!("joinGet('{db}.native_state', 'sec_floorheight', toUInt32(0))"),
        ceilingheight: &format!("joinGet('{db}.native_state', 'sec_ceilingheight', toUInt32(0))"),
    });
    let sql = format!(
        "SELECT rd.{} AS asks, toUInt32(rd.{}) AS draws\nFROM\n(\n    WITH\n{},\n{},\n    \
         ({list}) AS rd_asks\n    SELECT ({})[1] AS rd\n)",
        attacks::bombed::ASKS,
        attacks::bombed::DRAWS,
        constants(db),
        heights
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        attacks::radius_attack("rd_asks", &arrays.blast(), &hurting),
    );
    let ours: Bombed = fixture.scalar(&sql).await;
    let asks = ours
        .asks
        .into_iter()
        .map(|a| Bomb {
            target: a.0 as usize,
            inflictor: a.1 as usize,
            source: a.2 as usize,
            damage: i64::from(a.3),
            base: i64::from(a.4),
        })
        .collect();
    (asks, i64::from(ours.draws))
}
