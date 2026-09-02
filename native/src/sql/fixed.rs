//! The engine's fixed-point and angle arithmetic, as SQL expression text.
//!
//! Every function takes SQL expressions for its operands and returns one SQL
//! expression that computes what the C function of the same name computes,
//! bit for bit. A `fixed_t` is an `Int32` in 16.16, an `angle_t` a `UInt32`.
//! Callers pass expressions that already have those types; the results carry
//! them too, so the builders compose.
//!
//! Where the C relies on two's-complement wraparound, the text wraps
//! explicitly with `toInt32`/`toUInt32`, because ClickHouse widens `Int32`
//! arithmetic to `Int64`. `abs` of the most negative `Int32` stays that value,
//! as it does under gcc.

/// `FixedMul`: `(int64(a) * int64(b)) >> 16`.
pub fn fixed_mul(a: &str, b: &str) -> String {
    format!("toInt32(bitShiftRight(toInt64({a}) * toInt64({b}), 16))")
}

/// `FixedDiv`: saturates to `INT_MIN`/`INT_MAX` when `|a| >> 14 >= |b|`,
/// otherwise `(int64(a) << 16) / b` truncated toward zero.
pub fn fixed_div(a: &str, b: &str) -> String {
    let abs_a = abs32(a);
    let abs_b = abs32(b);
    format!(
        "if(bitShiftRight({abs_a}, 14) >= {abs_b}, \
         if(bitXor({a}, {b}) < 0, toInt32(-2147483648), toInt32(2147483647)), \
         toInt32(intDiv(bitShiftLeft(toInt64({a}), 16), toInt64({b}))))"
    )
}

/// C `abs` on an `int`: the most negative value maps to itself.
fn abs32(x: &str) -> String {
    format!("toInt32(abs(toInt64({x})))")
}

/// `SlopeDiv`: the `tantoangle` index for `num / den`, both unsigned, with
/// `num << 3` wrapping at 32 bits.
pub fn slope_div(num: &str, den: &str) -> String {
    format!(
        "if(toUInt32({den}) < 512, toUInt32(2048), \
         least(intDiv(toUInt32(toUInt64(toUInt32({num})) * 8), bitShiftRight(toUInt32({den}), 8)), toUInt32(2048)))"
    )
}

/// `R_PointToAngle` with the view point already subtracted: the binary angle
/// of `(dx, dy)` as a `UInt32`. `tantoangle` names a constant
/// `Array(UInt32)` holding the engine's table.
pub fn point_to_angle(dx: &str, dy: &str, tantoangle: &str) -> String {
    let ax = abs32(dx);
    let ay = abs32(dy);
    let t = |num: &str, den: &str| format!("{tantoangle}[1 + {}]", slope_div(num, den));
    let t_yx = t(&ay, &ax);
    let t_xy = t(&ax, &ay);
    format!(
        "multiIf({dx} = 0 AND {dy} = 0, toUInt32(0), \
         {dx} >= 0 AND {dy} >= 0 AND {dx} > {dy}, {t_yx}, \
         {dx} >= 0 AND {dy} >= 0, toUInt32(1073741823 - toInt64({t_xy})), \
         {dx} >= 0 AND {ax} > {ay}, toUInt32(4294967296 - toInt64({t_yx})), \
         {dx} >= 0, toUInt32(3221225472 + toInt64({t_xy})), \
         {dy} >= 0 AND {ax} > {ay}, toUInt32(2147483647 - toInt64({t_yx})), \
         {dy} >= 0, toUInt32(1073741824 + toInt64({t_xy})), \
         {ax} > {ay}, toUInt32(2147483648 + toInt64({t_yx})), \
         toUInt32(3221225471 - toInt64({t_xy})))"
    )
}

/// `P_AproxDistance`: `|dx| + |dy| - min(|dx|, |dy|) / 2`, wrapping.
pub fn aprox_distance(dx: &str, dy: &str) -> String {
    let ax = abs32(dx);
    let ay = abs32(dy);
    format!("toInt32(toInt64({ax}) + toInt64({ay}) - toInt64(bitShiftRight(least({ax}, {ay}), 1)))")
}

/// `R_PointOnSide`, `R_PointOnSegSide` and `P_PointOnDivlineSide` share this
/// shape: which side of the directed line through `(lx, ly)` with direction
/// `(ldx, ldy)` the point `(x, y)` lies on, 0 for the front, 1 for the back,
/// with the axis-aligned shortcuts and the sign-bit test. `shift` is what the
/// operands are pre-shifted by before `FixedMul`: 16 for nodes and segs, 8
/// for divlines.
pub fn point_on_side(
    x: &str,
    y: &str,
    lx: &str,
    ly: &str,
    ldx: &str,
    ldy: &str,
    shift: u32,
) -> String {
    let dx = format!("toInt32(toInt64({x}) - toInt64({lx}))");
    let dy = format!("toInt32(toInt64({y}) - toInt64({ly}))");
    let (left, right) = if shift == 16 {
        (
            fixed_mul(&format!("bitShiftRight({ldy}, 16)"), &dx),
            fixed_mul(&dy, &format!("bitShiftRight({ldx}, 16)")),
        )
    } else {
        (
            fixed_mul(
                &format!("bitShiftRight({ldy}, {shift})"),
                &format!("bitShiftRight({dx}, {shift})"),
            ),
            fixed_mul(
                &format!("bitShiftRight({dy}, {shift})"),
                &format!("bitShiftRight({ldx}, {shift})"),
            ),
        )
    };
    format!(
        "multiIf({ldx} = 0, if({x} <= {lx}, toUInt8({ldy} > 0), toUInt8({ldy} < 0)), \
         {ldy} = 0, if({y} <= {ly}, toUInt8({ldx} < 0), toUInt8({ldx} > 0)), \
         bitAnd(bitXor(bitXor(bitXor({ldy}, {ldx}), {dx}), {dy}), toInt32(-2147483648)) != 0, \
         toUInt8(bitAnd(bitXor({ldy}, {dx}), toInt32(-2147483648)) != 0), \
         toUInt8({right} >= {left}))"
    )
}

/// `P_PointOnLineSide`: no sign-bit shortcut, otherwise `point_on_side` with
/// a 16-bit pre-shift.
pub fn point_on_line_side(x: &str, y: &str, lx: &str, ly: &str, ldx: &str, ldy: &str) -> String {
    let dx = format!("toInt32(toInt64({x}) - toInt64({lx}))");
    let dy = format!("toInt32(toInt64({y}) - toInt64({ly}))");
    let left = fixed_mul(&format!("bitShiftRight({ldy}, 16)"), &dx);
    let right = fixed_mul(&dy, &format!("bitShiftRight({ldx}, 16)"));
    format!(
        "multiIf({ldx} = 0, if({x} <= {lx}, toUInt8({ldy} > 0), toUInt8({ldy} < 0)), \
         {ldy} = 0, if({y} <= {ly}, toUInt8({ldx} < 0), toUInt8({ldx} > 0)), \
         toUInt8({right} >= {left}))"
    )
}

/// `P_DivlineSide` from the sight code: 0 front, 1 back, 2 on the line, with
/// the engine's own comparison of `x` against the divline's `y` in the
/// horizontal case, kept because the frames it draws depend on it.
pub fn divline_side(x: &str, y: &str, lx: &str, ly: &str, ldx: &str, ldy: &str) -> String {
    let left = format!(
        "toInt32(toInt64(bitShiftRight({ldy}, 16)) * toInt64(bitShiftRight(toInt32(toInt64({x}) - toInt64({lx})), 16)))"
    );
    let right = format!(
        "toInt32(toInt64(bitShiftRight(toInt32(toInt64({y}) - toInt64({ly})), 16)) * toInt64(bitShiftRight({ldx}, 16)))"
    );
    format!(
        "multiIf({ldx} = 0, multiIf({x} = {lx}, toUInt8(2), {x} <= {lx}, toUInt8({ldy} > 0), toUInt8({ldy} < 0)), \
         {ldy} = 0, multiIf({x} = {ly}, toUInt8(2), {y} <= {ly}, toUInt8({ldx} < 0), toUInt8({ldx} > 0)), \
         {right} < {left}, toUInt8(0), \
         {left} = {right}, toUInt8(2), \
         toUInt8(1))"
    )
}

/// `P_InterceptVector(v2, v1)`: how far along `v2` the divline `v1` crosses
/// it, 0 when parallel.
#[allow(clippy::too_many_arguments)]
pub fn intercept_vector(
    v2x: &str,
    v2y: &str,
    v2dx: &str,
    v2dy: &str,
    v1x: &str,
    v1y: &str,
    v1dx: &str,
    v1dy: &str,
) -> String {
    let den = format!(
        "toInt32(toInt64({}) - toInt64({}))",
        fixed_mul(&format!("bitShiftRight({v1dy}, 8)"), v2dx),
        fixed_mul(&format!("bitShiftRight({v1dx}, 8)"), v2dy)
    );
    let num = format!(
        "toInt32(toInt64({}) + toInt64({}))",
        fixed_mul(
            &format!("bitShiftRight(toInt32(toInt64({v1x}) - toInt64({v2x})), 8)"),
            v1dy
        ),
        fixed_mul(
            &format!("bitShiftRight(toInt32(toInt64({v2y}) - toInt64({v1y})), 8)"),
            v1dx
        )
    );
    format!("if({den} = 0, toInt32(0), {})", fixed_div(&num, &den))
}

/// `R_PointToDist` with the view point already subtracted. `tantoangle` and
/// `finesine` name the constant arrays.
pub fn point_to_dist(dx: &str, dy: &str, tantoangle: &str, finesine: &str) -> String {
    let ax = abs32(dx);
    let ay = abs32(dy);
    let big = format!("greatest({ax}, {ay})");
    let small = format!("least({ax}, {ay})");
    let frac = format!("if({big} != 0, {}, toInt32(0))", fixed_div(&small, &big));
    let angle = format!(
        "bitShiftRight(toUInt32(toUInt64({tantoangle}[1 + bitShiftRight({frac}, 5)]) + 1073741824), 19)"
    );
    fixed_div(&big, &format!("{finesine}[1 + {angle}]"))
}

/// `R_ScaleFromGlobalAngle`: the wall scale at a screen angle, clamped to
/// `[256, 64 << 16]`.
pub fn scale_from_global_angle(
    visangle: &str,
    viewangle: &str,
    normalangle: &str,
    rw_distance: &str,
    projection: &str,
    finesine: &str,
) -> String {
    let sine = |other: &str| {
        format!(
            "{finesine}[1 + bitShiftRight(toUInt32(1073741824 + toUInt64(toUInt32(4294967296 + toUInt64({visangle}) - toUInt64({other})))), 19)]"
        )
    };
    let sinea = sine(viewangle);
    let sineb = sine(normalangle);
    let num = fixed_mul(projection, &sineb);
    let den = fixed_mul(rw_distance, &sinea);
    let scale = fixed_div(&num, &den);
    format!(
        "if({den} > bitShiftRight({num}, 16), least(greatest({scale}, toInt32(256)), toInt32(4194304)), toInt32(4194304))"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_mul_widens_then_narrows() {
        assert_eq!(
            fixed_mul("a", "b"),
            "toInt32(bitShiftRight(toInt64(a) * toInt64(b), 16))"
        );
    }

    #[test]
    fn fixed_div_saturates_before_it_divides() {
        let text = fixed_div("a", "b");
        assert!(text.starts_with(
            "if(bitShiftRight(toInt32(abs(toInt64(a))), 14) >= toInt32(abs(toInt64(b))),"
        ));
        assert!(text.contains("toInt32(-2147483648), toInt32(2147483647)"));
        assert!(text.ends_with("toInt32(intDiv(bitShiftLeft(toInt64(a), 16), toInt64(b))))"));
    }

    #[test]
    fn slope_div_wraps_the_shift_and_caps_the_answer() {
        let text = slope_div("n", "d");
        assert!(text.contains("toUInt32(toUInt64(toUInt32(n)) * 8)"));
        assert!(text.contains("toUInt32(2048)"));
    }

    #[test]
    fn every_builder_balances_its_parentheses() {
        let texts = [
            fixed_mul("a", "b"),
            fixed_div("a", "b"),
            slope_div("a", "b"),
            point_to_angle("dx", "dy", "T"),
            aprox_distance("dx", "dy"),
            point_on_side("x", "y", "lx", "ly", "ldx", "ldy", 16),
            point_on_side("x", "y", "lx", "ly", "ldx", "ldy", 8),
            point_on_line_side("x", "y", "lx", "ly", "ldx", "ldy"),
            divline_side("x", "y", "lx", "ly", "ldx", "ldy"),
            intercept_vector("a", "b", "c", "d", "e", "f", "g", "h"),
            point_to_dist("dx", "dy", "T", "S"),
            scale_from_global_angle("va", "vw", "na", "rd", "pr", "S"),
        ];
        for text in texts {
            let depth = text.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{text}");
        }
    }
}
