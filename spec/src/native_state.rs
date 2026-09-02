//! The native-mode state contract: what one game tic looks like as a row.
//!
//! The SQL simulation writes one row per tic into `native_state`, and the
//! reference emulator's probe writes rows of the same shape from the real
//! engine's memory. The differential between them is a column-wise comparison,
//! which only means something if both sides agree on the field list and its
//! order. That list lives here and nowhere else.
//!
//! Units follow the C engine: every `fixed_t` is an `Int32` in 16.16 form,
//! angles are `UInt32` binary angles, enums are their C integer values.
//! Pointers become identities: a thinker is named by the value of a global
//! counter taken when it was added, so list order is ascending identity.

/// Tics per second. The simulation advances one tic per row.
pub const TICRATE: u32 = 35;

/// Bumped whenever a field list below changes shape. Both writers stamp it.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// Scalars that describe the world as a whole.
pub const GAME_FIELDS: &[&str] = &[
    "leveltime",
    "prndindex",
    "rndindex",
    "next_seq",
    "next_linkseq",
    "paused",
    "demo_end",
    "totalkills",
    "totalitems",
    "totalsecret",
];

/// Per-mobj fields, one parallel array column each, indexed by slot in
/// thinker-list order. `m_target` and `m_tracer` hold identities, 0 for none.
pub const MOBJ_FIELDS: &[&str] = &[
    "m_id",
    "m_x",
    "m_y",
    "m_z",
    "m_angle",
    "m_sprite",
    "m_frame",
    "m_floorz",
    "m_ceilingz",
    "m_radius",
    "m_height",
    "m_momx",
    "m_momy",
    "m_momz",
    "m_type",
    "m_tics",
    "m_state",
    "m_flags",
    "m_health",
    "m_movedir",
    "m_movecount",
    "m_target",
    "m_reactiontime",
    "m_threshold",
    "m_player",
    "m_lastlook",
    "m_sp_x",
    "m_sp_y",
    "m_sp_angle",
    "m_sp_type",
    "m_sp_options",
    "m_tracer",
    "m_subsector",
    "m_linkseq",
];

/// Sector-thinker fields (doors, plats, floors, ceilings, lights), one
/// parallel array column each, indexed by slot in thinker-list order.
pub const SECTOR_THINKER_FIELDS: &[&str] = &[
    "s_seq",
    "s_kind",
    "s_sector",
    "s_type",
    "s_direction",
    "s_speed",
    "s_dest",
    "s_dest2",
    "s_count",
    "s_wait",
    "s_status",
    "s_oldstatus",
    "s_crush",
    "s_tag",
    "s_texture",
    "s_newspecial",
    "s_minlight",
    "s_maxlight",
    "s_mintime",
    "s_maxtime",
    "s_active",
    "s_activeplat_slot",
    "s_activeceil_slot",
];

/// The `s_kind` values.
pub mod sector_thinker_kind {
    pub const DOOR: u8 = 1;
    pub const PLAT: u8 = 2;
    pub const FLOOR: u8 = 3;
    pub const CEILING: u8 = 4;
    pub const LIGHT_FLASH: u8 = 5;
    pub const STROBE: u8 = 6;
    pub const GLOW: u8 = 7;
    pub const FIRE_FLICKER: u8 = 8;
}

/// Mutable per-sector fields, one array column each, indexed by sector number.
pub const SECTOR_FIELDS: &[&str] = &[
    "sec_floorheight",
    "sec_ceilingheight",
    "sec_floorpic",
    "sec_lightlevel",
    "sec_special",
    "sec_specialdata",
    "sec_soundtarget",
    "sec_soundtraversed",
];

/// Mutable line and side fields, indexed by line or side number.
pub const LINE_SIDE_FIELDS: &[&str] = &[
    "line_special",
    "side_toptexture",
    "side_midtexture",
    "side_bottomtexture",
    "side_textureoffset",
];

/// Switch-button timers, one array column each, `MAXBUTTONS` long.
pub const BUTTON_FIELDS: &[&str] = &["btn_line", "btn_where", "btn_texture", "btn_timer"];

/// Texture and flat animation tables, indexed by picture number.
pub const ANIM_FIELDS: &[&str] = &["texturetranslation", "flattranslation"];

/// Player one, scalar columns named after `player_t`.
pub const PLAYER_FIELDS: &[&str] = &[
    "p_mo",
    "p_playerstate",
    "p_cmd_forwardmove",
    "p_cmd_sidemove",
    "p_cmd_angleturn",
    "p_cmd_buttons",
    "p_viewz",
    "p_viewheight",
    "p_deltaviewheight",
    "p_bob",
    "p_health",
    "p_armorpoints",
    "p_armortype",
    "p_powers",
    "p_cards",
    "p_backpack",
    "p_readyweapon",
    "p_pendingweapon",
    "p_weaponowned",
    "p_ammo",
    "p_maxammo",
    "p_attackdown",
    "p_usedown",
    "p_cheats",
    "p_refire",
    "p_killcount",
    "p_itemcount",
    "p_secretcount",
    "p_message",
    "p_damagecount",
    "p_bonuscount",
    "p_attacker",
    "p_extralight",
    "p_fixedcolormap",
];

/// The two player sprites, one array column each, two entries long.
pub const PSPRITE_FIELDS: &[&str] = &["psp_state", "psp_tics", "psp_sx", "psp_sy"];

/// Status bar, heads-up display and menu statics that persist across tics.
pub const HUD_FIELDS: &[&str] = &[
    "st_faceindex",
    "st_facecount",
    "st_priority",
    "st_lastattackdown",
    "st_oldweaponsowned",
    "st_oldhealth",
    "st_randomnumber",
    "st_lastcalc",
    "st_calc_oldhealth",
    "st_palette",
    "st_clock",
    "hu_message_on",
    "hu_message_counter",
    "hu_message",
    "hu_nottobefuckedwith",
    "menu_skullanim",
    "menu_whichskull",
];

/// Interactive-input carry: how many tics a turn key has been held.
pub const INPUT_FIELDS: &[&str] = &["turnheld"];

/// Bits of the key-state word the driver streams once per tic. The SQL side
/// builds the tic command from these the way `G_BuildTiccmd` does.
pub mod key {
    pub const RIGHT: u32 = 1 << 0;
    pub const LEFT: u32 = 1 << 1;
    pub const UP: u32 = 1 << 2;
    pub const DOWN: u32 = 1 << 3;
    pub const FIRE: u32 = 1 << 4;
    pub const USE: u32 = 1 << 5;
    pub const STRAFE: u32 = 1 << 6;
    pub const SPEED: u32 = 1 << 7;
    pub const STRAFE_LEFT: u32 = 1 << 8;
    pub const STRAFE_RIGHT: u32 = 1 << 9;
    pub const PAUSE: u32 = 1 << 10;
    /// Weapon keys `1` to `7` occupy bits 16 to 22.
    pub const WEAPON_SHIFT: u32 = 16;
    pub const WEAPON_MASK: u32 = 0x7f << WEAPON_SHIFT;
}

/// Every field, in the order both writers emit them.
pub fn all_fields() -> Vec<&'static str> {
    [
        GAME_FIELDS,
        MOBJ_FIELDS,
        SECTOR_THINKER_FIELDS,
        SECTOR_FIELDS,
        LINE_SIDE_FIELDS,
        BUTTON_FIELDS,
        ANIM_FIELDS,
        PLAYER_FIELDS,
        PSPRITE_FIELDS,
        HUD_FIELDS,
        INPUT_FIELDS,
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn field_names_are_unique() {
        let all = all_fields();
        let set: HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "a field name repeats");
    }

    #[test]
    fn field_names_are_sql_identifiers() {
        for f in all_fields() {
            assert!(
                f.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{f} is not a plain identifier"
            );
        }
    }

    #[test]
    fn key_bits_do_not_overlap() {
        let bits = [
            key::RIGHT,
            key::LEFT,
            key::UP,
            key::DOWN,
            key::FIRE,
            key::USE,
            key::STRAFE,
            key::SPEED,
            key::STRAFE_LEFT,
            key::STRAFE_RIGHT,
            key::PAUSE,
        ];
        let mut seen = 0u32;
        for b in bits {
            assert_eq!(seen & b, 0);
            assert_eq!(b & key::WEAPON_MASK, 0);
            seen |= b;
        }
    }
}
