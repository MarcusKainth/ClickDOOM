//! The fixed-point SQL builders against a transliteration of the C.
//!
//! Every builder in `native::sql::fixed` is evaluated on the server over a
//! vector of inputs and compared with the same function written in Rust from
//! the engine's source. The Rust side exists only here: it is the oracle for
//! the text, and nothing ships it.

#![cfg(feature = "clickhouse-tests")]

mod support;

use clickdoom_native::sql::fixed;
use clickdoom_native::tables;
use support::db::Fixture;

/// A small linear congruential generator, so the vectors are the same on
/// every run and the test needs no randomness source.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn i32(&mut self) -> i32 {
        self.next() as i32
    }

    /// Mostly map-sized values, with the odd extreme.
    fn fixed(&mut self) -> i32 {
        match self.next() % 8 {
            0 => self.i32(),
            1 => (self.next() % 65536) as i32 - 32768,
            _ => (self.next() % (4096 << 16)) as i32 - (2048 << 16),
        }
    }
}

// The engine's arithmetic, from m_fixed.c, tables.c, r_main.c, p_maputl.c
// and p_sight.c, in Rust.

fn c_abs(x: i32) -> i32 {
    x.wrapping_abs()
}

fn fixed_mul(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b)) >> 16) as i32
}

fn fixed_div(a: i32, b: i32) -> i32 {
    if (c_abs(a) >> 14) >= c_abs(b) {
        if (a ^ b) < 0 { i32::MIN } else { i32::MAX }
    } else {
        ((i64::from(a) << 16) / i64::from(b)) as i32
    }
}

fn slope_div(num: u32, den: u32) -> u32 {
    if den < 512 {
        2048
    } else {
        (num.wrapping_shl(3) / (den >> 8)).min(2048)
    }
}

fn point_to_angle(dx: i32, dy: i32, tantoangle: &[u32]) -> u32 {
    let t = |num: i32, den: i32| tantoangle[slope_div(num as u32, den as u32) as usize];
    if dx == 0 && dy == 0 {
        return 0;
    }
    if dx >= 0 {
        if dy >= 0 {
            if dx > dy {
                t(dy, dx)
            } else {
                0x4000_0000u32.wrapping_sub(1).wrapping_sub(t(dx, dy))
            }
        } else {
            let y = c_abs(dy);
            if dx > y {
                0u32.wrapping_sub(t(y, dx))
            } else {
                0xc000_0000u32.wrapping_add(t(dx, y))
            }
        }
    } else {
        let x = c_abs(dx);
        if dy >= 0 {
            if x > dy {
                0x8000_0000u32.wrapping_sub(1).wrapping_sub(t(dy, x))
            } else {
                0x4000_0000u32.wrapping_add(t(x, dy))
            }
        } else {
            let y = c_abs(dy);
            if x > y {
                0x8000_0000u32.wrapping_add(t(y, x))
            } else {
                0xc000_0000u32.wrapping_sub(1).wrapping_sub(t(x, y))
            }
        }
    }
}

fn aprox_distance(dx: i32, dy: i32) -> i32 {
    let (dx, dy) = (c_abs(dx), c_abs(dy));
    if dx < dy {
        dx.wrapping_add(dy).wrapping_sub(dx >> 1)
    } else {
        dx.wrapping_add(dy).wrapping_sub(dy >> 1)
    }
}

fn point_on_side(x: i32, y: i32, lx: i32, ly: i32, ldx: i32, ldy: i32, shift: u32) -> u8 {
    if ldx == 0 {
        return if x <= lx {
            u8::from(ldy > 0)
        } else {
            u8::from(ldy < 0)
        };
    }
    if ldy == 0 {
        return if y <= ly {
            u8::from(ldx < 0)
        } else {
            u8::from(ldx > 0)
        };
    }
    let dx = x.wrapping_sub(lx);
    let dy = y.wrapping_sub(ly);
    if (ldy ^ ldx ^ dx ^ dy) & i32::MIN != 0 {
        return u8::from((ldy ^ dx) & i32::MIN != 0);
    }
    let (left, right) = if shift == 16 {
        (fixed_mul(ldy >> 16, dx), fixed_mul(dy, ldx >> 16))
    } else {
        (
            fixed_mul(ldy >> shift, dx >> shift),
            fixed_mul(dy >> shift, ldx >> shift),
        )
    };
    u8::from(right >= left)
}

fn point_on_line_side(x: i32, y: i32, lx: i32, ly: i32, ldx: i32, ldy: i32) -> u8 {
    if ldx == 0 {
        return if x <= lx {
            u8::from(ldy > 0)
        } else {
            u8::from(ldy < 0)
        };
    }
    if ldy == 0 {
        return if y <= ly {
            u8::from(ldx < 0)
        } else {
            u8::from(ldx > 0)
        };
    }
    let dx = x.wrapping_sub(lx);
    let dy = y.wrapping_sub(ly);
    u8::from(fixed_mul(dy, ldx >> 16) >= fixed_mul(ldy >> 16, dx))
}

fn divline_side(x: i32, y: i32, lx: i32, ly: i32, ldx: i32, ldy: i32) -> u8 {
    if ldx == 0 {
        if x == lx {
            return 2;
        }
        return if x <= lx {
            u8::from(ldy > 0)
        } else {
            u8::from(ldy < 0)
        };
    }
    if ldy == 0 {
        if x == ly {
            return 2;
        }
        return if y <= ly {
            u8::from(ldx < 0)
        } else {
            u8::from(ldx > 0)
        };
    }
    let dx = x.wrapping_sub(lx);
    let dy = y.wrapping_sub(ly);
    let left = (ldy >> 16).wrapping_mul(dx >> 16);
    let right = (dy >> 16).wrapping_mul(ldx >> 16);
    if right < left {
        0
    } else if left == right {
        2
    } else {
        1
    }
}

#[allow(clippy::too_many_arguments)]
fn intercept_vector(
    v2x: i32,
    v2y: i32,
    v2dx: i32,
    v2dy: i32,
    v1x: i32,
    v1y: i32,
    v1dx: i32,
    v1dy: i32,
) -> i32 {
    let den = fixed_mul(v1dy >> 8, v2dx).wrapping_sub(fixed_mul(v1dx >> 8, v2dy));
    if den == 0 {
        return 0;
    }
    let num = fixed_mul(v1x.wrapping_sub(v2x) >> 8, v1dy)
        .wrapping_add(fixed_mul(v2y.wrapping_sub(v1y) >> 8, v1dx));
    fixed_div(num, den)
}

fn point_to_dist(dx: i32, dy: i32, tantoangle: &[u32], finesine: &[i32]) -> i32 {
    let (mut dx, mut dy) = (c_abs(dx), c_abs(dy));
    if dy > dx {
        std::mem::swap(&mut dx, &mut dy);
    }
    let frac = if dx != 0 { fixed_div(dy, dx) } else { 0 };
    let angle = (tantoangle[(frac >> 5) as usize].wrapping_add(0x4000_0000)) >> 19;
    fixed_div(dx, finesine[angle as usize])
}

fn scale_from_global_angle(
    visangle: u32,
    viewangle: u32,
    normalangle: u32,
    rw_distance: i32,
    projection: i32,
    finesine: &[i32],
) -> i32 {
    let anglea = 0x4000_0000u32.wrapping_add(visangle.wrapping_sub(viewangle));
    let angleb = 0x4000_0000u32.wrapping_add(visangle.wrapping_sub(normalangle));
    let sinea = finesine[(anglea >> 19) as usize];
    let sineb = finesine[(angleb >> 19) as usize];
    let num = fixed_mul(projection, sineb);
    let den = fixed_mul(rw_distance, sinea);
    if den > num >> 16 {
        fixed_div(num, den).clamp(256, 64 << 16)
    } else {
        64 << 16
    }
}

/// An array literal, cast as a whole so the text stays short: the vectors
/// and the engine's tables add up to well over the parser's default limit.
fn array_literal<T: std::fmt::Display>(values: &[T], ty: &str) -> String {
    let items: Vec<String> = values.iter().map(|v| v.to_string()).collect();
    format!("[{}]::Array({ty})", items.join(","))
}

/// Evaluates `expr` (written over the lambda parameters `a0..aN`) on the
/// server for every row of `columns`, and returns the results.
async fn evaluate<T>(
    fixture: &Fixture,
    with: &str,
    expr: &str,
    columns: &[(String, &str)],
) -> Vec<T>
where
    T: clickhouse::RowOwned + clickhouse::RowRead,
{
    let params: Vec<String> = (0..columns.len()).map(|i| format!("a{i}")).collect();
    let arrays: Vec<String> = columns
        .iter()
        .map(|(values, ty)| format!("{values}::Array({ty})"))
        .collect();
    let sql = format!(
        "{with} SELECT v FROM (SELECT arrayMap(({}) -> {expr}, {}) AS vs) ARRAY JOIN vs AS v",
        params.join(", "),
        arrays.join(", ")
    );
    fixture
        .db
        .clone()
        .with_setting("max_query_size", "16000000")
        .query(&sql)
        .fetch_all::<T>()
        .await
        .unwrap_or_else(|e| panic!("{}: {e}", &sql[..sql.len().min(200)]))
}

#[derive(clickhouse::Row, serde::Deserialize)]
struct I32Row {
    v: i32,
}
#[derive(clickhouse::Row, serde::Deserialize)]
struct U32Row {
    v: u32,
}
#[derive(clickhouse::Row, serde::Deserialize)]
struct U8Row {
    v: u8,
}

fn constants() -> (Vec<u32>, Vec<i32>, String) {
    let tantoangle: Vec<u32> = tables::table("tantoangle")
        .unwrap()
        .ints("value")
        .unwrap()
        .iter()
        .map(|&v| v as u32)
        .collect();
    let finesine: Vec<i32> = tables::table("finesine")
        .unwrap()
        .ints("value")
        .unwrap()
        .iter()
        .map(|&v| v as i32)
        .collect();
    let with = format!(
        "WITH {} AS T, {} AS S",
        array_literal(&tantoangle, "UInt32"),
        array_literal(&finesine, "Int32")
    );
    (tantoangle, finesine, with)
}

const N: usize = 4000;

#[tokio::test]
async fn the_sql_arithmetic_matches_the_engine_bit_for_bit() {
    let fixture = Fixture::create("fixed").await;
    let (tantoangle, finesine, with) = constants();
    let mut rng = Lcg(0x5eed_d00d);
    let a: Vec<i32> = (0..N).map(|_| rng.fixed()).collect();
    let b: Vec<i32> = (0..N).map(|_| rng.fixed()).collect();
    // Edge cases the generator would rarely hit.
    let mut a = a;
    let mut b = b;
    for (x, y) in [
        (0, 1),
        (1, 0),
        (i32::MIN, 1),
        (i32::MAX, -1),
        (i32::MIN, i32::MIN),
        (65536, 3),
        (-65536, 3),
        (1, i32::MIN),
        (7, -2),
        (-7, 2),
    ] {
        a.push(x);
        b.push(y);
    }
    let cols_ab = [
        (array_literal(&a, "Int32"), "Int32"),
        (array_literal(&b, "Int32"), "Int32"),
    ];

    let got: Vec<I32Row> = evaluate(&fixture, "", &fixed::fixed_mul("a0", "a1"), &cols_ab).await;
    for (i, row) in got.iter().enumerate() {
        assert_eq!(row.v, fixed_mul(a[i], b[i]), "FixedMul({}, {})", a[i], b[i]);
    }

    // FixedDiv divides by zero only where the C would, so keep b non-zero.
    let b_nz: Vec<i32> = b.iter().map(|&v| if v == 0 { 1 } else { v }).collect();
    let cols_div = [
        (array_literal(&a, "Int32"), "Int32"),
        (array_literal(&b_nz, "Int32"), "Int32"),
    ];
    let got: Vec<I32Row> = evaluate(&fixture, "", &fixed::fixed_div("a0", "a1"), &cols_div).await;
    for (i, row) in got.iter().enumerate() {
        assert_eq!(
            row.v,
            fixed_div(a[i], b_nz[i]),
            "FixedDiv({}, {})",
            a[i],
            b_nz[i]
        );
    }

    // Angles and distances take map coordinates, a few thousand units at
    // most; the tables they index have no entry for the extremes above.
    let bound = |v: i32| v.clamp(-0x3fff_ffff, 0x3fff_ffff);
    let ma: Vec<i32> = a.iter().map(|&v| bound(v)).collect();
    let mb: Vec<i32> = b.iter().map(|&v| bound(v)).collect();
    let cols_map = [
        (array_literal(&ma, "Int32"), "Int32"),
        (array_literal(&mb, "Int32"), "Int32"),
    ];
    let got: Vec<U32Row> = evaluate(
        &fixture,
        &with,
        &fixed::point_to_angle("a0", "a1", "T"),
        &cols_map,
    )
    .await;
    for (i, row) in got.iter().enumerate() {
        assert_eq!(
            row.v,
            point_to_angle(ma[i], mb[i], &tantoangle),
            "R_PointToAngle({}, {})",
            ma[i],
            mb[i]
        );
    }

    let got: Vec<I32Row> =
        evaluate(&fixture, "", &fixed::aprox_distance("a0", "a1"), &cols_ab).await;
    for (i, row) in got.iter().enumerate() {
        assert_eq!(
            row.v,
            aprox_distance(a[i], b[i]),
            "P_AproxDistance({}, {})",
            a[i],
            b[i]
        );
    }

    let got: Vec<I32Row> = evaluate(
        &fixture,
        &with,
        &fixed::point_to_dist("a0", "a1", "T", "S"),
        &cols_map,
    )
    .await;
    for (i, row) in got.iter().enumerate() {
        assert_eq!(
            row.v,
            point_to_dist(ma[i], mb[i], &tantoangle, &finesine),
            "R_PointToDist({}, {})",
            ma[i],
            mb[i]
        );
    }
    fixture.finish().await;
}

#[tokio::test]
async fn the_side_tests_match_the_engine() {
    let fixture = Fixture::create("sides").await;
    let mut rng = Lcg(0x0bad_cafe);
    let cols: Vec<Vec<i32>> = (0..6)
        .map(|k| {
            (0..N)
                .map(|_| {
                    if k >= 4 && rng.next().is_multiple_of(5) {
                        0
                    } else {
                        rng.fixed()
                    }
                })
                .collect()
        })
        .collect();
    let literals: Vec<(String, &str)> = cols
        .iter()
        .map(|c| (array_literal(c, "Int32"), "Int32"))
        .collect();
    let args = ["a0", "a1", "a2", "a3", "a4", "a5"];
    let row = |i: usize| {
        (
            cols[0][i], cols[1][i], cols[2][i], cols[3][i], cols[4][i], cols[5][i],
        )
    };

    for shift in [16u32, 8] {
        let got: Vec<U8Row> = evaluate(
            &fixture,
            "",
            &fixed::point_on_side(args[0], args[1], args[2], args[3], args[4], args[5], shift),
            &literals,
        )
        .await;
        for (i, r) in got.iter().enumerate() {
            let (x, y, lx, ly, ldx, ldy) = row(i);
            assert_eq!(
                r.v,
                point_on_side(x, y, lx, ly, ldx, ldy, shift),
                "point_on_side shift {shift} row {i}"
            );
        }
    }
    let got: Vec<U8Row> = evaluate(
        &fixture,
        "",
        &fixed::point_on_line_side(args[0], args[1], args[2], args[3], args[4], args[5]),
        &literals,
    )
    .await;
    for (i, r) in got.iter().enumerate() {
        let (x, y, lx, ly, ldx, ldy) = row(i);
        assert_eq!(
            r.v,
            point_on_line_side(x, y, lx, ly, ldx, ldy),
            "P_PointOnLineSide row {i}"
        );
    }
    let got: Vec<U8Row> = evaluate(
        &fixture,
        "",
        &fixed::divline_side(args[0], args[1], args[2], args[3], args[4], args[5]),
        &literals,
    )
    .await;
    for (i, r) in got.iter().enumerate() {
        let (x, y, lx, ly, ldx, ldy) = row(i);
        assert_eq!(
            r.v,
            divline_side(x, y, lx, ly, ldx, ldy),
            "P_DivlineSide row {i}"
        );
    }
    fixture.finish().await;
}

#[tokio::test]
async fn intercepts_and_wall_scales_match_the_engine() {
    let fixture = Fixture::create("intercept").await;
    let (_, finesine, with) = constants();
    let mut rng = Lcg(0x1234_5678);
    let cols: Vec<Vec<i32>> = (0..8)
        .map(|_| (0..N).map(|_| rng.fixed()).collect())
        .collect();
    let literals: Vec<(String, &str)> = cols
        .iter()
        .map(|c| (array_literal(c, "Int32"), "Int32"))
        .collect();
    let got: Vec<I32Row> = evaluate(
        &fixture,
        "",
        &fixed::intercept_vector("a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7"),
        &literals,
    )
    .await;
    for (i, r) in got.iter().enumerate() {
        let c = |k: usize| cols[k][i];
        assert_eq!(
            r.v,
            intercept_vector(c(0), c(1), c(2), c(3), c(4), c(5), c(6), c(7)),
            "P_InterceptVector row {i}"
        );
    }

    let visangle: Vec<u32> = (0..N).map(|_| rng.next()).collect();
    let viewangle: Vec<u32> = (0..N).map(|_| rng.next()).collect();
    let normalangle: Vec<u32> = (0..N).map(|_| rng.next()).collect();
    let rw_distance: Vec<i32> = (0..N)
        .map(|_| (rng.next() % (3000 << 16)) as i32 + 1)
        .collect();
    let projection = 160 << 16;
    let scale_cols = [
        (array_literal(&visangle, "UInt32"), "UInt32"),
        (array_literal(&viewangle, "UInt32"), "UInt32"),
        (array_literal(&normalangle, "UInt32"), "UInt32"),
        (array_literal(&rw_distance, "Int32"), "Int32"),
    ];
    let expr = fixed::scale_from_global_angle(
        "a0",
        "a1",
        "a2",
        "a3",
        &format!("toInt32({projection})"),
        "S",
    );
    let got: Vec<I32Row> = evaluate(&fixture, &with, &expr, &scale_cols).await;
    for (i, r) in got.iter().enumerate() {
        assert_eq!(
            r.v,
            scale_from_global_angle(
                visangle[i],
                viewangle[i],
                normalangle[i],
                rw_distance[i],
                projection,
                &finesine
            ),
            "R_ScaleFromGlobalAngle row {i}"
        );
    }
    fixture.finish().await;
}
