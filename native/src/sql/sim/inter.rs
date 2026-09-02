//! Picking things up, from `p_inter.c`.
//!
//! `P_TouchSpecialThing` is one switch on the thing's sprite, and each arm
//! either takes the thing or leaves it lying there. A move can touch
//! several things, so the switch is folded over what the move touched and
//! appears once.

/// `p_inter.c`
const BONUSADD: i64 = 6;
const CLIPAMMO: [i64; 4] = [10, 4, 20, 1];
/// `p_local.h`
const MAXHEALTH: i64 = 100;
/// `deh_misc.h`
const MAX_HEALTH: i64 = 200;
const MAX_ARMOR: i64 = 200;
const SOULSPHERE_HEALTH: i64 = 100;
const MAX_SOULSPHERE: i64 = 200;
const GREEN_ARMOR_CLASS: i64 = 1;
const BLUE_ARMOR_CLASS: i64 = 2;

/// `doomdef.h`: how long each power lasts.
const TICRATE: i64 = 35;
const INVULNTICS: i64 = 30 * TICRATE;
const INVISTICS: i64 = 60 * TICRATE;
const INFRATICS: i64 = 120 * TICRATE;
const IRONTICS: i64 = 60 * TICRATE;

/// `p_mobj.h`
const MF_COUNTITEM: i64 = 0x80_0000;
const MF_DROPPED: i64 = 0x2_0000;
const MF_SHADOW: i64 = 0x4_0000;

/// `doomdef.h`: the ammo a weapon draws, one-based for the array it sits
/// in. `am_noammo` is past the end.
const AM_CLIP: usize = 1;
const AM_SHELL: usize = 2;
const AM_CELL: usize = 3;
const AM_MISL: usize = 4;

/// `doomdef.h`: the weapons, one-based for `p_weaponowned`.
const WP_FIST: i64 = 0;
const WP_PISTOL: i64 = 1;
const WP_SHOTGUN: i64 = 2;
const WP_CHAINGUN: i64 = 3;
const WP_MISSILE: i64 = 4;
const WP_PLASMA: i64 = 5;
const WP_BFG: i64 = 6;
const WP_CHAINSAW: i64 = 7;

/// `doomdef.h`: the powers, one-based for `p_powers`.
const PW_INVULNERABILITY: usize = 1;
const PW_STRENGTH: usize = 2;
const PW_INVISIBILITY: usize = 3;
const PW_IRONFEET: usize = 4;
const PW_ALLMAP: usize = 5;
const PW_INFRARED: usize = 6;

/// `doomdef.h`: the cards, one-based for `p_cards`.
const IT_BLUECARD: usize = 1;
const IT_YELLOWCARD: usize = 2;
const IT_REDCARD: usize = 3;

/// The accumulator the fold threads, member by member. The order is the
/// order `state()` builds it and `field()` reads it.
mod slot {
    pub const HEALTH: usize = 1;
    pub const ARMORPOINTS: usize = 2;
    pub const ARMORTYPE: usize = 3;
    pub const AMMO: usize = 4;
    pub const MAXAMMO: usize = 5;
    pub const BACKPACK: usize = 6;
    pub const CARDS: usize = 7;
    pub const POWERS: usize = 8;
    pub const WEAPONOWNED: usize = 9;
    pub const PENDINGWEAPON: usize = 10;
    pub const MESSAGE: usize = 11;
    pub const ITEMCOUNT: usize = 12;
    pub const BONUSCOUNT: usize = 13;
    pub const SHADOW: usize = 14;
    pub const TAKEN: usize = 15;
}

/// Where the slots a fold took sit in its accumulator.
pub const TAKEN: usize = slot::TAKEN;

/// What the player carries into a move, as expressions.
pub struct Player<'a> {
    pub health: &'a str,
    pub armorpoints: &'a str,
    pub armortype: &'a str,
    pub ammo: &'a str,
    pub maxammo: &'a str,
    pub backpack: &'a str,
    pub cards: &'a str,
    pub powers: &'a str,
    pub weaponowned: &'a str,
    pub pendingweapon: &'a str,
    pub message: &'a str,
    pub itemcount: &'a str,
    pub bonuscount: &'a str,
    /// The mobj's flags, which invisibility adds to.
    pub mo_flags: &'a str,
}

/// The starting accumulator, before anything is touched.
pub fn start(player: &Player<'_>) -> String {
    format!(
        "(toInt32({}), toInt32({}), toInt32({}), {}, {}, toUInt8({}), {}, {}, {}, \
         toInt32({}), toUInt64({}), toInt32({}), toInt32({}), \
         toUInt8(bitAnd({}, {MF_SHADOW}) != 0), CAST([], 'Array(UInt32)'))",
        player.health,
        player.armorpoints,
        player.armortype,
        player.ammo,
        player.maxammo,
        player.backpack,
        player.cards,
        player.powers,
        player.weaponowned,
        player.pendingweapon,
        player.message,
        player.itemcount,
        player.bonuscount,
        player.mo_flags,
    )
}

/// What an arm of the switch decides: which `P_Give*` to call, its two
/// arguments, and what to tell the player.
///
/// The switch is on the thing's sprite and has an arm per pickup; the call
/// it decides on is made once, after it. Keeping the two apart is what
/// keeps the arms narrow, because an arm that built the whole player would
/// be one wide branch out of thirty.
mod gift {
    pub const KIND: usize = 1;
    pub const A: usize = 2;
    pub const B: usize = 3;
    pub const MESSAGE: usize = 4;
}

/// The calls the arms decide between.
mod give {
    pub const NOTHING: i64 = 0;
    pub const ARMOR: i64 = 1;
    pub const HEALTH_BONUS: i64 = 2;
    pub const ARMOR_BONUS: i64 = 3;
    pub const SOUL: i64 = 4;
    pub const CARD: i64 = 5;
    pub const BODY: i64 = 6;
    pub const POWER: i64 = 7;
    pub const AMMO: i64 = 8;
    pub const BACKPACK: i64 = 9;
    pub const WEAPON: i64 = 10;
}

/// `P_TouchSpecialThing` over the slots a move touched, in order.
///
/// `touched` names the array of mobj slots, `into` the accumulator this
/// starts from. `sprite`, `flags` and `z` are the mobj arrays.
#[allow(clippy::too_many_arguments)]
pub fn touch(
    touched: &str,
    into: &str,
    sprite: &str,
    flags: &str,
    m_z: &str,
    toucher_z: &str,
    toucher_height: &str,
    skill: &str,
) -> String {
    let reach = format!(
        "toInt64({m_z}[k]) - toInt64({toucher_z}) <= toInt64({toucher_height}) \
         AND toInt64({m_z}[k]) - toInt64({toucher_z}) >= {}",
        -(8i64 << 16)
    );
    let values = vec![
        ("pk_gift".to_owned(), arms(sprite, flags)),
        ("pk_took".to_owned(), took()),
        ("pk_after".to_owned(), applied(skill)),
    ];
    // The tail of the switch: the thing is counted, removed, and adds to
    // the bonus flash.
    let taken = format!(
        "(pk_after.{h}, pk_after.{ap}, pk_after.{at}, pk_after.{am}, pk_after.{mx}, \
         pk_after.{bp}, pk_after.{cd}, pk_after.{pw}, pk_after.{wo}, pk_after.{pd}, \
         pk_after.{msg}, \
         toInt32(pk_after.{ic} + if(bitAnd({flags}[k], {MF_COUNTITEM}) != 0, 1, 0)), \
         toInt32(pk_after.{bc} + {BONUSADD}), pk_after.{sh}, \
         arrayPushBack(pk_after.{tk}, toUInt32(k)))",
        h = slot::HEALTH,
        ap = slot::ARMORPOINTS,
        at = slot::ARMORTYPE,
        am = slot::AMMO,
        mx = slot::MAXAMMO,
        bp = slot::BACKPACK,
        cd = slot::CARDS,
        pw = slot::POWERS,
        wo = slot::WEAPONOWNED,
        pd = slot::PENDINGWEAPON,
        msg = slot::MESSAGE,
        ic = slot::ITEMCOUNT,
        bc = slot::BONUSCOUNT,
        sh = slot::SHADOW,
        tk = slot::TAKEN,
    );
    let body = format!("if(pk_took = 0, acc, {taken})");
    format!(
        "arrayFold((acc, k) -> if(NOT ({reach}) OR acc.{h} <= 0, acc, {}), {touched}, {into})",
        crate::sql::bind::chain(&values, &body),
        h = slot::HEALTH,
    )
}

/// The switch on the thing's sprite: which call to make, with what, and
/// what to say. A sprite no arm names is what `P_SpecialThing` calls
/// `I_Error` for, and it takes nothing.
fn arms(sprite: &str, flags: &str) -> String {
    let s = format!("{sprite}[k]");
    let dropped = format!("if(bitAnd({flags}[k], {MF_DROPPED}) != 0, 1, 0)");
    let mut arms: Vec<String> = Vec::new();
    let mut push = |name: &str, kind: i64, a: String, b: String, text: &str| {
        arms.push(format!(
            "{s} = sprnum['{name}'], (toInt64({kind}), toInt64({a}), toInt64({b}), {})",
            message(text)
        ));
    };
    let none = "0".to_owned();
    push(
        "ARM1",
        give::ARMOR,
        GREEN_ARMOR_CLASS.to_string(),
        none.clone(),
        "Picked up the armor.",
    );
    push(
        "ARM2",
        give::ARMOR,
        BLUE_ARMOR_CLASS.to_string(),
        none.clone(),
        "Picked up the MegaArmor!",
    );
    push(
        "BON1",
        give::HEALTH_BONUS,
        none.clone(),
        none.clone(),
        "Picked up a health bonus.",
    );
    push(
        "BON2",
        give::ARMOR_BONUS,
        none.clone(),
        none.clone(),
        "Picked up an armor bonus.",
    );
    push(
        "SOUL",
        give::SOUL,
        none.clone(),
        none.clone(),
        "Supercharge!",
    );
    push(
        "BKEY",
        give::CARD,
        IT_BLUECARD.to_string(),
        none.clone(),
        "Picked up a blue keycard.",
    );
    push(
        "YKEY",
        give::CARD,
        IT_YELLOWCARD.to_string(),
        none.clone(),
        "Picked up a yellow keycard.",
    );
    push(
        "RKEY",
        give::CARD,
        IT_REDCARD.to_string(),
        none.clone(),
        "Picked up a red keycard.",
    );
    push(
        "STIM",
        give::BODY,
        "10".to_owned(),
        none.clone(),
        "Picked up a stimpack.",
    );
    push(
        "MEDI",
        give::BODY,
        "25".to_owned(),
        "1".to_owned(),
        "Picked up a medikit.",
    );
    push(
        "PINV",
        give::POWER,
        PW_INVULNERABILITY.to_string(),
        INVULNTICS.to_string(),
        "Invulnerability!",
    );
    push(
        "PSTR",
        give::POWER,
        PW_STRENGTH.to_string(),
        "1".to_owned(),
        "Berserk!",
    );
    push(
        "PINS",
        give::POWER,
        PW_INVISIBILITY.to_string(),
        INVISTICS.to_string(),
        "Partial Invisibility",
    );
    push(
        "SUIT",
        give::POWER,
        PW_IRONFEET.to_string(),
        IRONTICS.to_string(),
        "Radiation Shielding Suit",
    );
    push(
        "PMAP",
        give::POWER,
        PW_ALLMAP.to_string(),
        "1".to_owned(),
        "Computer Area Map",
    );
    push(
        "PVIS",
        give::POWER,
        PW_INFRARED.to_string(),
        INFRATICS.to_string(),
        "Light Amplification Visor",
    );
    push(
        "CLIP",
        give::AMMO,
        AM_CLIP.to_string(),
        format!("1 - {dropped}"),
        "Picked up a clip.",
    );
    push(
        "AMMO",
        give::AMMO,
        AM_CLIP.to_string(),
        "5".to_owned(),
        "Picked up a box of bullets.",
    );
    push(
        "ROCK",
        give::AMMO,
        AM_MISL.to_string(),
        "1".to_owned(),
        "Picked up a rocket.",
    );
    push(
        "BROK",
        give::AMMO,
        AM_MISL.to_string(),
        "5".to_owned(),
        "Picked up a box of rockets.",
    );
    push(
        "CELL",
        give::AMMO,
        AM_CELL.to_string(),
        "1".to_owned(),
        "Picked up an energy cell.",
    );
    push(
        "CELP",
        give::AMMO,
        AM_CELL.to_string(),
        "5".to_owned(),
        "Picked up an energy cell pack.",
    );
    push(
        "SHEL",
        give::AMMO,
        AM_SHELL.to_string(),
        "1".to_owned(),
        "Picked up 4 shotgun shells.",
    );
    push(
        "SBOX",
        give::AMMO,
        AM_SHELL.to_string(),
        "5".to_owned(),
        "Picked up a box of shotgun shells.",
    );
    push(
        "BPAK",
        give::BACKPACK,
        none.clone(),
        none.clone(),
        "Picked up a backpack full of ammo!",
    );
    push(
        "BFUG",
        give::WEAPON,
        WP_BFG.to_string(),
        "2".to_owned(),
        "You got the BFG9000!  Oh, yes.",
    );
    push(
        "MGUN",
        give::WEAPON,
        WP_CHAINGUN.to_string(),
        format!("2 - {dropped}"),
        "You got the chaingun!",
    );
    push(
        "CSAW",
        give::WEAPON,
        WP_CHAINSAW.to_string(),
        "2".to_owned(),
        "A chainsaw!  Find some meat!",
    );
    push(
        "LAUN",
        give::WEAPON,
        WP_MISSILE.to_string(),
        "2".to_owned(),
        "You got the rocket launcher!",
    );
    push(
        "PLAS",
        give::WEAPON,
        WP_PLASMA.to_string(),
        "2".to_owned(),
        "You got the plasma gun!",
    );
    push(
        "SHOT",
        give::WEAPON,
        WP_SHOTGUN.to_string(),
        format!("2 - {dropped}"),
        "You got the shotgun!",
    );
    push(
        "SGN2",
        give::WEAPON,
        "8".to_owned(),
        format!("2 - {dropped}"),
        "You got the super shotgun!",
    );
    format!(
        "multiIf({}, (toInt64({}), toInt64(0), toInt64(0), toUInt64(0)))",
        arms.join(", "),
        give::NOTHING
    )
}

/// Whether the call the arm decided on takes the thing.
///
/// Each `P_Give*` has its own answer to that, and the ones that always
/// take say so.
fn took() -> String {
    let kind = format!("pk_gift.{}", gift::KIND);
    let a = format!("pk_gift.{}", gift::A);
    let hits = format!("{a} * 100");
    let ammo = format!("1 + {a}");
    format!(
        "toUInt8(multiIf(\
         {kind} = {}, 0, \
         {kind} = {}, acc.{ap} < {hits}, \
         {kind} = {}, acc.{h} < {MAXHEALTH}, \
         {kind} = {}, acc.{pw}[{a}] = 0 OR {a} != {PW_ALLMAP}, \
         {kind} = {}, acc.{am}[{a}] != acc.{mx}[{a}], \
         {kind} = {}, acc.{wo}[1 + {a}] = 0 OR (weapon_ammo[1 + {a}] < 4 \
         AND acc.{am}[{ammo_of}] != acc.{mx}[{ammo_of}]), \
         1))",
        give::NOTHING,
        give::ARMOR,
        give::BODY,
        give::POWER,
        give::AMMO,
        give::WEAPON,
        ap = slot::ARMORPOINTS,
        h = slot::HEALTH,
        pw = slot::POWERS,
        am = slot::AMMO,
        mx = slot::MAXAMMO,
        wo = slot::WEAPONOWNED,
        ammo_of = format!("1 + weapon_ammo[{ammo}]"),
    )
}

/// The `P_Give*` the arm decided on, applied to the accumulator.
fn applied(skill: &str) -> String {
    let kind = format!("pk_gift.{}", gift::KIND);
    let a = format!("pk_gift.{}", gift::A);
    let b = format!("pk_gift.{}", gift::B);
    let text = format!("pk_gift.{}", gift::MESSAGE);
    let doubled = format!("if({skill} = 0 OR {skill} = 4, 1, 0)");
    let at = |array: usize, index: &str, value: &str| {
        format!(
            "arrayMap((v, i) -> toInt32(if(i = {index}, {value}, v)), acc.{array}, \
             arrayEnumerate(acc.{array}))"
        )
    };
    let clip = format!("clipammo[{a}]");
    let amount = format!("bitShiftLeft(if({b} != 0, {b} * {clip}, intDiv({clip}, 2)), {doubled})");
    let weapon_ammo = format!("1 + weapon_ammo[1 + {a}]");
    let weapon_amount = format!("bitShiftLeft({b} * clipammo[{weapon_ammo}], {doubled})");
    let members = [
        // health
        format!(
            "toInt32(multiIf({kind} = {}, least(acc.{h} + 1, {MAX_HEALTH}), \
             {kind} = {}, least(acc.{h} + {SOULSPHERE_HEALTH}, {MAX_SOULSPHERE}), \
             {kind} = {} AND {a} = {PW_STRENGTH}, greatest(acc.{h}, {MAXHEALTH}), \
             {kind} = {}, least(acc.{h} + {a}, {MAXHEALTH}), acc.{h}))",
            give::HEALTH_BONUS,
            give::SOUL,
            give::POWER,
            give::BODY,
            h = slot::HEALTH
        ),
        // armorpoints
        format!(
            "toInt32(multiIf({kind} = {}, {a} * 100, \
             {kind} = {}, least(acc.{ap} + 1, {MAX_ARMOR}), acc.{ap}))",
            give::ARMOR,
            give::ARMOR_BONUS,
            ap = slot::ARMORPOINTS
        ),
        // armortype
        format!(
            "toInt32(multiIf({kind} = {}, {a}, \
             {kind} = {} AND acc.{at} = 0, 1, acc.{at}))",
            give::ARMOR,
            give::ARMOR_BONUS,
            at = slot::ARMORTYPE
        ),
        // ammo
        format!(
            "multiIf({kind} = {}, {}, {kind} = {} AND weapon_ammo[1 + {a}] < 4, {}, \
             {kind} = {}, {}, acc.{am})",
            give::AMMO,
            at(
                slot::AMMO,
                &a,
                &format!("least(v + {amount}, acc.{}[{a}])", slot::MAXAMMO)
            ),
            give::WEAPON,
            at(
                slot::AMMO,
                &weapon_ammo,
                &format!(
                    "least(v + {weapon_amount}, acc.{}[{weapon_ammo}])",
                    slot::MAXAMMO
                )
            ),
            give::BACKPACK,
            backpack_ammo(&doubled),
            am = slot::AMMO
        ),
        // maxammo
        format!(
            "if({kind} = {} AND acc.{bp} = 0, arrayMap(v -> toInt32(v * 2), acc.{mx}), acc.{mx})",
            give::BACKPACK,
            bp = slot::BACKPACK,
            mx = slot::MAXAMMO
        ),
        // backpack
        format!(
            "toUInt8(if({kind} = {}, 1, acc.{bp}))",
            give::BACKPACK,
            bp = slot::BACKPACK
        ),
        // cards
        format!(
            "if({kind} = {}, arrayMap((v, i) -> toUInt8(if(i = {a}, 1, v)), acc.{cd}, \
             arrayEnumerate(acc.{cd})), acc.{cd})",
            give::CARD,
            cd = slot::CARDS
        ),
        // powers
        format!(
            "if({kind} = {}, {}, acc.{pw})",
            give::POWER,
            at(slot::POWERS, &a, &b),
            pw = slot::POWERS
        ),
        // weaponowned
        format!(
            "if({kind} = {}, {}, acc.{wo})",
            give::WEAPON,
            at(slot::WEAPONOWNED, &format!("1 + {a}"), "1"),
            wo = slot::WEAPONOWNED
        ),
        // pendingweapon
        format!(
            "toInt32(multiIf({kind} = {} AND acc.{wo}[1 + {a}] = 0, {a}, \
             {kind} = {} AND {a} = {PW_STRENGTH} AND pk_readyweapon != {WP_FIST}, {WP_FIST}, \
             {kind} = {} AND acc.{am}[{a}] = 0, {}, acc.{pd}))",
            give::WEAPON,
            give::POWER,
            give::AMMO,
            next_weapon(&a),
            wo = slot::WEAPONOWNED,
            am = slot::AMMO,
            pd = slot::PENDINGWEAPON
        ),
        // message
        format!(
            "toUInt64(multiIf(\
             {kind} = {} AND {b} = 1 AND least(acc.{h} + {a}, {MAXHEALTH}) < 25, {}, \
             {kind} = {} AND acc.{cd}[{a}] != 0, acc.{msg}, {text}))",
            give::BODY,
            message("Picked up a medikit that you REALLY need!"),
            give::CARD,
            h = slot::HEALTH,
            cd = slot::CARDS,
            msg = slot::MESSAGE
        ),
        format!("acc.{}", slot::ITEMCOUNT),
        // bonuscount: a new card sets it rather than adding to it
        format!(
            "toInt32(if({kind} = {} AND acc.{cd}[{a}] = 0, {BONUSADD}, acc.{bc}))",
            give::CARD,
            cd = slot::CARDS,
            bc = slot::BONUSCOUNT
        ),
        // shadow
        format!(
            "toUInt8(if({kind} = {} AND {a} = {PW_INVISIBILITY}, 1, acc.{sh}))",
            give::POWER,
            sh = slot::SHADOW
        ),
        format!("acc.{}", slot::TAKEN),
    ];
    format!("({})", members.join(", "))
}

/// The backpack gives a clip of every kind, against the doubled maximum.
fn backpack_ammo(doubled: &str) -> String {
    let maxima = format!(
        "if(acc.{bp} = 0, arrayMap(v -> toInt32(v * 2), acc.{mx}), acc.{mx})",
        bp = slot::BACKPACK,
        mx = slot::MAXAMMO
    );
    format!(
        "arrayMap((v, i) -> toInt32(least(v + bitShiftLeft(clipammo[i], {doubled}), \
         ({maxima})[i])), acc.{am}, arrayEnumerate(acc.{am}))",
        am = slot::AMMO
    )
}

/// The weapon `P_GiveAmmo` puts up when the player had none of that ammo.
///
/// It reads the ready weapon, which no pickup moves, so the caller binds
/// it as `pk_readyweapon` once.
fn next_weapon(ammo: &str) -> String {
    let owns = |w: i64| format!("acc.{wo}[1 + {w}] != 0", wo = slot::WEAPONOWNED);
    format!(
        "multiIf(\
         {ammo} = {AM_CLIP} AND pk_readyweapon = {WP_FIST}, if({}, {WP_CHAINGUN}, {WP_PISTOL}), \
         {ammo} = {AM_SHELL} AND (pk_readyweapon = {WP_FIST} OR pk_readyweapon = {WP_PISTOL}) \
         AND {}, {WP_SHOTGUN}, \
         {ammo} = {AM_CELL} AND (pk_readyweapon = {WP_FIST} OR pk_readyweapon = {WP_PISTOL}) \
         AND {}, {WP_PLASMA}, \
         {ammo} = {AM_MISL} AND pk_readyweapon = {WP_FIST} AND {}, {WP_MISSILE}, \
         acc.{pd})",
        owns(WP_CHAINGUN),
        owns(WP_SHOTGUN),
        owns(WP_PLASMA),
        owns(WP_MISSILE),
        pd = slot::PENDINGWEAPON
    )
}

/// A message, hashed the way both writers hash one.
fn message(text: &str) -> String {
    format!("xxHash64('{}')", text.replace('\'', "''"))
}

/// The constants the switch reads.
pub fn constants(db: &str) -> Vec<(String, String)> {
    vec![
        (
            "sprnum".to_owned(),
            format!(
                "(SELECT mapFromArrays(groupArray(name), groupArray(toInt32(id)))\
                 \n     FROM {db}.sprnames)"
            ),
        ),
        (
            "weapon_ammo".to_owned(),
            super::table_column(db, "weaponinfo", "ammo"),
        ),
        (
            "clipammo".to_owned(),
            format!(
                "CAST([{}], 'Array(Int64)')",
                CLIPAMMO
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> Player<'static> {
        Player {
            health: "p_h",
            armorpoints: "p_ap",
            armortype: "p_at",
            ammo: "p_am",
            maxammo: "p_mx",
            backpack: "p_bp",
            cards: "p_cd",
            powers: "p_pw",
            weaponowned: "p_wo",
            pendingweapon: "p_pd",
            message: "p_msg",
            itemcount: "p_ic",
            bonuscount: "p_bc",
            mo_flags: "p_fl",
        }
    }

    #[test]
    fn the_starting_accumulator_has_one_member_per_slot() {
        let text = start(&player());
        assert_eq!(text.matches("p_").count(), 14);
    }

    #[test]
    fn a_clip_asks_for_ammo_and_a_sprite_no_arm_names_asks_for_nothing() {
        let text = arms("m_sprite", "m_flags");
        assert!(text.contains("xxHash64('Picked up a clip.')"));
        assert!(text.contains("sprnum['CLIP']"));
        assert!(text.ends_with(", (toInt64(0), toInt64(0), toInt64(0), toUInt64(0)))"));
    }

    #[test]
    fn each_arm_decides_and_one_call_applies_it() {
        let text = arms("m_sprite", "m_flags");
        // Thirty-two sprites, each a four-member decision rather than a
        // whole player.
        assert_eq!(text.matches("sprnum[").count(), 32);
        assert!(!text.contains("arrayMap"), "an arm builds no array");
    }

    #[test]
    fn every_builder_balances_its_parentheses() {
        let texts = [
            start(&player()),
            touch(
                "hit", "into", "m_sprite", "m_flags", "m_z", "tz", "th", "skill",
            ),
        ];
        for text in texts {
            let depth = text.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0);
        }
    }
}
