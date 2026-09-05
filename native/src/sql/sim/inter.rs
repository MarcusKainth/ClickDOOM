//! Picking things up, from `p_inter.c`.
//!
//! `P_TouchSpecialThing` is one switch on the thing's sprite, and each arm
//! either takes the thing or leaves it lying there. A move can touch
//! several things, so the switch is folded over what the move touched and
//! appears once.
//!
//! `P_DamageMobj` and `P_KillMobj` are here too: what a shot or a monster's
//! own attack does to what it reaches.

use crate::sql::{bind, fixed};

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
///
/// `weapon_ammo` is not here. `pspr::constants` binds it for the weapon
/// the player holds, and one name is bound once.
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

// ---------------------------------------------------------------------------
// Damage
// ---------------------------------------------------------------------------

/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
const MF_JUSTHIT: i64 = 64;
const MF_NOGRAVITY: i64 = 512;
const MF_DROPOFF: i64 = 0x400;
const MF_NOCLIP: i64 = 0x1000;
const MF_FLOAT: i64 = 0x4000;
const MF_CORPSE: i64 = 0x10_0000;
const MF_COUNTKILL: i64 = 0x40_0000;
const MF_SKULLFLY: i64 = 0x100_0000;
/// `p_local.h`: how long a thing chases what hit it before it looks
/// elsewhere.
const BASETHRESHOLD: i64 = 100;
/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const ANG180: i64 = 0x8000_0000;
const ANGLE_WRAP: i64 = 1 << 32;
const ANGLETOFINESHIFT: u32 = 19;
/// `p_inter.c`: how far below the thing that hit it a target has to stand
/// to be knocked over, and the most damage that can do it.
const FALL_HEIGHT: i64 = 64 * FRACUNIT;
const FALL_DAMAGE: i64 = 40;

/// Where each field of a damage ask sits in its tuple.
pub mod hurting {
    /// The mobj slot taking the damage.
    pub const TARGET: usize = 1;
    /// The slot the damage arrived from, 0 for none. The push and the fall
    /// read where it stands. A hitscan passes the shooter for this and for
    /// [`SOURCE`] both.
    pub const INFLICTOR: usize = 2;
    /// The slot the hit is credited to, 0 for none. The chainsaw test, the
    /// chase and the kill count read it.
    pub const SOURCE: usize = 3;
    pub const DAMAGE: usize = 4;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 5;
}

/// Where each field of a damage answer sits in its tuple.
pub mod hurt {
    pub const HEALTH: usize = 1;
    pub const FLAGS: usize = 2;
    pub const STATE: usize = 3;
    pub const TICS: usize = 4;
    pub const MOMX: usize = 5;
    pub const MOMY: usize = 6;
    pub const MOMZ: usize = 7;
    pub const HEIGHT: usize = 8;
    pub const REACTIONTIME: usize = 9;
    pub const TARGET: usize = 10;
    pub const THRESHOLD: usize = 11;
    /// 1 where the thing died.
    pub const KILLED: usize = 12;
    /// 1 where the death adds to the kill count.
    pub const COUNTED: usize = 13;
    /// The thing type the death drops, -1 for none.
    pub const DROP: usize = 14;
    /// How many numbers the call drew.
    pub const DRAWS: usize = 15;
    /// 1 where the call reached a path this does not write.
    pub const STUCK: usize = 16;
}

/// A call nobody made, for a caller that reads the first answer of a list
/// that may be empty.
pub fn no_hurt() -> String {
    "(toInt32(0), toInt32(0), toInt32(0), toInt32(0), toInt32(0), toInt32(0), toInt32(0), \
     toInt32(0), toInt32(0), toUInt32(0), toInt32(0), toUInt8(0), toUInt8(0), toInt32(-1), \
     toUInt32(0), toUInt8(0))"
        .to_owned()
}

/// The arrays a damage call reads.
pub struct Hurting<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_z: &'a str,
    pub m_momx: &'a str,
    pub m_momy: &'a str,
    pub m_momz: &'a str,
    pub m_reactiontime: &'a str,
    pub m_type: &'a str,
    pub m_state: &'a str,
    pub m_tics: &'a str,
    pub m_flags: &'a str,
    pub m_health: &'a str,
    pub m_height: &'a str,
    pub m_target: &'a str,
    pub m_threshold: &'a str,
    pub m_player: &'a str,
    pub prndindex: &'a str,
    /// The weapon in the player's hands, which is what decides whether a
    /// hit pushes its target.
    pub readyweapon: &'a str,
}

/// The engine tables a damage call reads that no other stage does.
pub fn damage_constants(db: &str) -> Vec<(String, String)> {
    let kind = |name: &str| {
        format!("assumeNotNull((SELECT toInt32(id) FROM {db}.mobjtype WHERE name = '{name}'))")
    };
    let info = |column: &str| super::table_column(db, "mobjinfo", column);
    let mut constants = vec![
        ("mobj_mass".to_owned(), info("mass")),
        ("mobj_painchance".to_owned(), info("painchance")),
        ("mobj_painstate".to_owned(), info("painstate")),
        ("mobj_deathstate".to_owned(), info("deathstate")),
        ("mobj_xdeathstate".to_owned(), info("xdeathstate")),
    ];
    for name in ["A_Pain", "A_Scream"] {
        constants.push((
            name.to_lowercase(),
            format!("assumeNotNull((SELECT id FROM {db}.action_functions WHERE name = '{name}'))"),
        ));
    }
    // `MT_SKULL` and `MT_VILE` are not here. `enemy::constants` binds both
    // for `P_CheckMissileRange`, and a name bound twice in one `WITH` list
    // is one whose value depends on which binding the server keeps.
    for name in [
        "MT_POSSESSED",
        "MT_WOLFSS",
        "MT_SHOTGUY",
        "MT_CHAINGUY",
        "MT_CLIP",
        "MT_SHOTGUN",
        "MT_CHAINGUN",
    ] {
        constants.push((name.to_lowercase(), kind(name)));
    }
    constants
}

/// `P_DamageMobj` over every ask in `asks`, as a [`hurt`] tuple each.
///
/// A call draws once, for the pain chance where the thing lives and for the
/// wait on its death frame where it does not, and once more where the hit
/// may knock it over. Both are decided before either number is read, so a
/// caller making several calls knows the offset each one's draws sit at.
///
/// A player target leaves the call stuck rather than guessed: the armour,
/// the damage tint and the weapon it drops are the player's own columns.
pub fn damage_mobj(asks: &str, world: &Hurting<'_>) -> String {
    let (values, body) = damaged(world);
    format!(
        "arrayMap(dm_ask -> {}, {asks})",
        bind::chain_in("dma", &values, &body)
    )
}

/// How many numbers each ask in `asks` draws.
///
/// A caller making several calls in a row needs each one's offset before
/// any of them is worked out. Nothing a call draws changes how many draws
/// it makes, so the count is this much of the routine and no more.
pub fn draws(asks: &str, world: &Hurting<'_>) -> String {
    let body = "toUInt32(if(dm_lands = 1, 1 + toUInt32(dm_may_fall), 0))";
    format!(
        "arrayMap(dm_ask -> {}, {asks})",
        bind::chain_in("dmd", &reach(world), body)
    )
}

/// What a call works out before it reads a number: whether the damage
/// lands, whether it pushes, and whether it may knock the target over.
fn reach(world: &Hurting<'_>) -> Vec<(String, String)> {
    let a = |field: usize| format!("dm_ask.{field}");
    let at = |array: &str| format!("{array}[dm_target]");
    let from = |array: &str| format!("{array}[dm_inflictor]");
    let credited = |array: &str| format!("{array}[dm_source]");
    vec![
        (
            "dm_target".to_owned(),
            format!("toUInt32({})", a(hurting::TARGET)),
        ),
        (
            "dm_inflictor".to_owned(),
            format!("toUInt32({})", a(hurting::INFLICTOR)),
        ),
        (
            "dm_source".to_owned(),
            format!("toUInt32({})", a(hurting::SOURCE)),
        ),
        (
            "dm_damage".to_owned(),
            format!("toInt32({})", a(hurting::DAMAGE)),
        ),
        (
            "dm_flags".to_owned(),
            format!("toInt32({})", at(world.m_flags)),
        ),
        (
            "dm_health".to_owned(),
            format!("toInt32({})", at(world.m_health)),
        ),
        // The two early returns: a thing that cannot be shot, and one
        // already dead, take nothing and draw nothing.
        (
            "dm_lands".to_owned(),
            format!("toUInt8(bitAnd(dm_flags, {MF_SHOOTABLE}) != 0 AND dm_health > 0)"),
        ),
        // The push. A call with no inflictor pushes nothing, and a
        // chainsaw in the source's hands holds its target in reach.
        (
            "dm_pushes".to_owned(),
            format!(
                "toUInt8(dm_lands = 1 AND dm_inflictor != 0 AND bitAnd(dm_flags, {MF_NOCLIP}) = 0 \
                 AND (dm_source = 0 OR {} = -1 OR {} != {WP_CHAINSAW}))",
                credited(world.m_player),
                world.readyweapon,
            ),
        ),
        // Falling forwards is the one draw a call makes before the damage
        // lands, and whether it is made is decided without reading it.
        (
            "dm_may_fall".to_owned(),
            format!(
                "toUInt8(dm_pushes = 1 AND dm_damage < {FALL_DAMAGE} AND dm_damage > dm_health \
                 AND toInt64({}) - toInt64({}) > {FALL_HEIGHT})",
                at(world.m_z),
                from(world.m_z),
            ),
        ),
    ]
}

/// [`damage_mobj`] over an ask list that carries at most one, folded
/// rather than mapped.
///
/// A map runs every function in its body once even on an empty list, and
/// this body is the whole routine. A fold runs its body only where the
/// list has an element, so a caller with nothing to hurt pays for the fold
/// and nothing under it. The answer is the last ask in the list, and
/// [`no_hurt`] is what an empty one gives.
pub fn damage_fold(asks: &str, world: &Hurting<'_>) -> String {
    let (values, body) = damaged(world);
    format!(
        "arrayFold((dm_held, dm_ask) -> {}, {asks}, {})",
        bind::chain_in("dma", &values, &body),
        no_hurt(),
    )
}

/// What one call works out, as the values a body reads and the [`hurt`]
/// tuple it answers with.
fn damaged(world: &Hurting<'_>) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("dm_ask.{field}");
    let at = |array: &str| format!("{array}[dm_target]");
    let from = |array: &str| format!("{array}[dm_inflictor]");
    let credited = |array: &str| format!("{array}[dm_source]");
    let info = |table: &str| format!("{table}[1 + dm_type]");
    let mut values: Vec<(String, String)> = reach(world);
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("dm_type", format!("toInt32({})", at(world.m_type)));
    // A lost soul charging stops dead where it is hit, and the push below
    // then reads the momentum it stopped at.
    value(
        "dm_flying",
        format!("toUInt8(dm_lands = 1 AND bitAnd(dm_flags, {MF_SKULLFLY}) != 0)"),
    );
    value(
        "dm_momx_held",
        format!("toInt32(if(dm_flying = 1, 0, {}))", at(world.m_momx)),
    );
    value(
        "dm_momy_held",
        format!("toInt32(if(dm_flying = 1, 0, {}))", at(world.m_momy)),
    );
    value(
        "dm_thrust",
        format!(
            "toInt32(intDiv(dm_damage * {} * 100, {}))",
            FRACUNIT >> 3,
            info("mobj_mass")
        ),
    );
    value(
        "dm_angle",
        fixed::point_to_angle(
            &format!(
                "toInt32(toInt64({}) - toInt64({}))",
                at(world.m_x),
                from(world.m_x)
            ),
            &format!(
                "toInt32(toInt64({}) - toInt64({}))",
                at(world.m_y),
                from(world.m_y)
            ),
            "tantoangle",
        ),
    );
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + {nth}, 255)])",
            world.prndindex,
            a(hurting::BASE),
        )
    };
    value(
        "dm_falls",
        format!("toUInt8(dm_may_fall = 1 AND bitAnd({}, 1) != 0)", draw("1")),
    );
    value(
        "dm_fine",
        format!(
            "toUInt32(bitShiftRight(bitAnd(toUInt64(dm_angle) + if(dm_falls = 1, {ANG180}, 0), \
             {}), {ANGLETOFINESHIFT}))",
            ANGLE_WRAP - 1
        ),
    );
    value(
        "dm_push",
        "toInt32(if(dm_falls = 1, dm_thrust * 4, dm_thrust))".to_owned(),
    );
    let along = |wave: String, held: &str| {
        format!(
            "toInt32(toInt64({held}) + if(dm_pushes = 1, toInt64({}), 0))",
            fixed::fixed_mul("dm_push", &wave)
        )
    };
    value(
        "dm_momx",
        along(super::maputl::finecosine("dm_fine"), "dm_momx_held"),
    );
    value(
        "dm_momy",
        along(super::maputl::finesine("dm_fine"), "dm_momy_held"),
    );
    // The damage, and the death it may cause.
    value("dm_left", "toInt32(dm_health - dm_damage)".to_owned());
    value(
        "dm_killed",
        "toUInt8(dm_lands = 1 AND dm_left <= 0)".to_owned(),
    );
    // The second draw sits behind the fall's, where one was made.
    let second = format!(
        "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + 1 + toUInt32(dm_may_fall), 255)])",
        world.prndindex,
        a(hurting::BASE),
    );
    // `P_KillMobj`: what a corpse carries, how far it falls and the frame
    // it dies in.
    value(
        "dm_corpse_flags",
        format!(
            "toInt32(bitOr(bitAnd(dm_flags, if(dm_type != mt_skull, {}, {})), {}))",
            !(MF_SHOOTABLE | MF_FLOAT | MF_SKULLFLY | MF_NOGRAVITY),
            !(MF_SHOOTABLE | MF_FLOAT | MF_SKULLFLY),
            MF_CORPSE | MF_DROPOFF,
        ),
    );
    value(
        "dm_death_state",
        format!(
            "toInt32(if(dm_left < -{} AND {} != 0, {}, {}))",
            info("mobj_spawnhealth"),
            info("mobj_xdeathstate"),
            info("mobj_xdeathstate"),
            info("mobj_deathstate"),
        ),
    );
    value(
        "dm_drop",
        "toInt32(multiIf(dm_type = mt_possessed OR dm_type = mt_wolfss, mt_clip, \
         dm_type = mt_shotguy, mt_shotgun, dm_type = mt_chainguy, mt_chaingun, -1))"
            .to_owned(),
    );
    // The pain frame, which the second draw decides for a thing that
    // lives.
    value(
        "dm_pained",
        format!(
            "toUInt8(dm_lands = 1 AND dm_killed = 0 AND {second} < {} AND dm_flying = 0)",
            info("mobj_painchance")
        ),
    );
    // The chase after whatever hit it. An archvile's target is never taken
    // off it, and one is never chased.
    value(
        "dm_chases",
        format!(
            "toUInt8(dm_lands = 1 AND dm_killed = 0 \
             AND ({} = 0 OR dm_type = mt_vile) \
             AND dm_source != 0 AND dm_source != dm_target AND {} != mt_vile)",
            at(world.m_threshold),
            credited(world.m_type),
        ),
    );
    // The chase reads the frame the thing stands in after the pain frame
    // has been entered, so a thing that was in its spawn frame and is
    // pained does not go on to its see frame.
    value(
        "dm_after_pain",
        format!(
            "toInt32(if(dm_pained = 1, {}, {}))",
            info("mobj_painstate"),
            at(world.m_state),
        ),
    );
    value(
        "dm_wakes",
        format!(
            "toUInt8(dm_chases = 1 AND dm_after_pain = {} AND {} != 0)",
            info("mobj_spawnstate"),
            info("mobj_seestate"),
        ),
    );
    // The engine sets the pain frame and then the see frame, so a thing
    // that does both ends in the see frame.
    value(
        "dm_state",
        format!(
            "toInt32(multiIf(dm_lands = 0, {held}, dm_killed = 1, dm_death_state, \
             dm_wakes = 1, {}, dm_pained = 1, {}, {held}))",
            info("mobj_seestate"),
            info("mobj_painstate"),
            held = at(world.m_state),
        ),
    );
    // A frame the routine enters brings its own wait, whether or not it is
    // the frame the thing already stood in.
    value(
        "dm_moves",
        "toUInt8(dm_killed = 1 OR dm_wakes = 1 OR dm_pained = 1)".to_owned(),
    );
    value(
        "dm_tics",
        format!(
            "toInt32(multiIf(dm_moves = 0, {held}, \
             dm_killed = 1, greatest(state_tics[1 + dm_death_state] - bitAnd({second}, 3), 1), \
             state_tics[1 + dm_state]))",
            held = at(world.m_tics),
        ),
    );
    // A player target leaves the call stuck: the armour it wears, the tint
    // it takes and the weapon it drops are the player's own columns.
    // `P_SetMobjState` runs the routine the frame it enters carries.
    // `A_Pain` and `A_Scream` only make a noise; any other leaves the call
    // stuck rather than guessed, which is what an `A_Chase` on a see frame
    // and an `A_Explode` on a barrel's death frame do.
    value(
        "dm_routine",
        "toInt32(if(dm_moves = 1, state_action[1 + dm_state], 0))".to_owned(),
    );
    value(
        "dm_stuck",
        format!(
            "toUInt8(dm_lands = 1 AND ({} != -1 \
             OR (dm_routine != 0 AND dm_routine != a_pain AND dm_routine != a_scream)))",
            at(world.m_player)
        ),
    );
    let members = [
        "toInt32(if(dm_lands = 1, dm_left, dm_health))".to_owned(),
        format!(
            "toInt32(multiIf(dm_killed = 1, dm_corpse_flags, \
             dm_pained = 1, bitOr(dm_flags, {MF_JUSTHIT}), dm_flags))"
        ),
        "toInt32(dm_state)".to_owned(),
        "toInt32(dm_tics)".to_owned(),
        "toInt32(dm_momx)".to_owned(),
        "toInt32(dm_momy)".to_owned(),
        format!("toInt32(if(dm_flying = 1, 0, {}))", at(world.m_momz)),
        format!(
            "toInt32(if(dm_killed = 1, bitShiftRight({}, 2), {held}))",
            at(world.m_height),
            held = at(world.m_height),
        ),
        format!(
            "toInt32(if(dm_lands = 1 AND dm_killed = 0, 0, {}))",
            at(world.m_reactiontime)
        ),
        format!(
            "toUInt32(if(dm_chases = 1, dm_source, {}))",
            at(world.m_target)
        ),
        format!(
            "toInt32(if(dm_chases = 1, {BASETHRESHOLD}, {}))",
            at(world.m_threshold)
        ),
        "toUInt8(dm_killed)".to_owned(),
        format!("toUInt8(dm_killed = 1 AND bitAnd(dm_flags, {MF_COUNTKILL}) != 0)"),
        "toInt32(if(dm_killed = 1, dm_drop, -1))".to_owned(),
        "toUInt32(if(dm_lands = 1, 1 + toUInt32(dm_may_fall), 0))".to_owned(),
        "toUInt8(dm_stuck)".to_owned(),
    ];
    (values, format!("({})", members.join(", ")))
}

#[cfg(test)]
mod damage_tests {
    use super::*;

    fn world() -> Hurting<'static> {
        Hurting {
            m_x: "m_x",
            m_y: "m_y",
            m_z: "m_z",
            m_momx: "m_momx",
            m_momy: "m_momy",
            m_momz: "m_momz",
            m_reactiontime: "m_reactiontime",
            m_type: "m_type",
            m_state: "m_state",
            m_tics: "m_tics",
            m_flags: "m_flags",
            m_health: "m_health",
            m_height: "m_height",
            m_target: "m_target",
            m_threshold: "m_threshold",
            m_player: "m_player",
            prndindex: "prndindex",
            readyweapon: "readyweapon",
        }
    }

    /// A call draws once where it lands and once more where the hit may
    /// knock its target over, and nothing where it does not land. Every
    /// draw after it in the tic sits behind that count.
    #[test]
    fn a_call_draws_by_where_it_lands_and_whether_it_may_fell() {
        let (_, body) = damaged(&world());
        assert!(
            body.contains("toUInt32(if(dm_lands = 1, 1 + toUInt32(dm_may_fall), 0))"),
            "{body}"
        );
    }

    /// The push angle and the fall test read where the inflictor stands;
    /// the chainsaw test and the chase read the source.
    #[test]
    fn the_push_reads_the_inflictor_and_the_credit_reads_the_source() {
        let (values, _) = damaged(&world());
        let named = |name: &str| {
            values
                .iter()
                .find(|(held, _)| held == name)
                .map(|(_, expr)| expr.clone())
                .unwrap_or_else(|| panic!("the call names {name}"))
        };
        let angle = named("dm_angle");
        assert!(angle.contains("m_x[dm_inflictor]"), "{angle}");
        assert!(!angle.contains("dm_source"), "{angle}");
        let falls = named("dm_may_fall");
        assert!(falls.contains("m_z[dm_inflictor]"), "{falls}");
        let pushes = named("dm_pushes");
        assert!(pushes.contains("dm_inflictor != 0"), "{pushes}");
        assert!(pushes.contains("m_player[dm_source]"), "{pushes}");
        let chases = named("dm_chases");
        assert!(chases.contains("m_type[dm_source]"), "{chases}");
        assert!(!chases.contains("dm_inflictor"), "{chases}");
    }

    /// A player target leaves the call stuck rather than guessed, because
    /// the armour, the tint and the dropped weapon are the player's own
    /// columns.
    #[test]
    fn a_player_target_leaves_the_call_stuck() {
        let (values, _) = damaged(&world());
        let stuck = values
            .iter()
            .find(|(name, _)| name == "dm_stuck")
            .expect("the call names what leaves it stuck");
        assert!(stuck.1.contains("m_player[dm_target] != -1"), "{stuck:?}");
    }

    /// The routine reads no thing type it names by hand out of the
    /// generator: every one comes from `mobjtype` inside the statement.
    /// Two of them are bound by `enemy::constants` rather than here.
    #[test]
    fn every_thing_type_it_names_comes_from_the_table() {
        let sql = damage_mobj("asks", &world());
        let mut named: Vec<String> = damage_constants("nat")
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("mt_"))
            .collect();
        named.extend(["mt_skull".to_owned(), "mt_vile".to_owned()]);
        assert!(named.len() >= 9, "{named:?}");
        for name in &named {
            assert!(sql.contains(name), "{name}");
        }
        let bound: Vec<String> = super::super::constants("nat")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for name in &named {
            assert!(bound.contains(name), "the statement binds {name}");
        }
    }

    /// The frames a call enters carry five routines between them across
    /// the whole of `mobjinfo`, and the two this runs are the two that only
    /// make a noise. The rest leave the call stuck, so this records what
    /// the allow-list has to cover and fails if a table change adds to it.
    #[test]
    fn the_routines_a_call_can_meet_are_the_ones_it_answers_for() {
        let states = crate::tables::table("states").unwrap();
        let action = states.ints("action").unwrap();
        let names = crate::tables::table("action_functions").unwrap();
        let named = |id: i64| {
            let ids = names.ints("id").unwrap();
            let at = ids.iter().position(|held| *held == id).expect("a routine");
            names.texts("name").unwrap()[at]
        };
        let info = crate::tables::table("mobjinfo").unwrap();
        let mut met: Vec<&str> = Vec::new();
        for column in ["painstate", "deathstate", "xdeathstate", "seestate"] {
            for state in info.ints(column).unwrap() {
                let routine = action[state as usize];
                if state != 0 && routine != 0 && !met.contains(&named(routine)) {
                    met.push(named(routine));
                }
            }
        }
        met.sort_unstable();
        assert_eq!(
            met,
            [
                "A_BrainAwake",
                "A_BrainPain",
                "A_BrainScream",
                "A_Chase",
                "A_Explode",
                "A_Hoof",
                "A_Metal",
                "A_Pain",
                "A_Scream",
                "A_VileChase",
            ]
        );
    }

    #[test]
    fn the_damage_expression_balances_its_parentheses() {
        let sql = damage_mobj("asks", &world());
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0, "{sql}");
    }
}
