-- The DDL native mode runs against.
--
-- `{{DB}}` is the database name, substituted by `native::sql::schema_statements`.
-- Every statement is idempotent, so re-running the schema against a
-- provisioned database changes nothing.
--
-- Three kinds of table live here, and the difference is what may write them.
--
--   * `wad_lumps` and the constant tables carry raw bytes the driver
--     inserts: lumps as they lie in the file, and the engine's own
--     initializers as `native/tables/` holds them.
--   * Everything named `lv_*`, `tex_*`, `sprite_*` and the rest of the
--     asset tables is derived, and only SQL writes them. Nothing outside
--     ClickHouse decodes a map record, composes a texture or resolves a
--     name to a number.
--   * `native_state` and `native_frames` are the simulation's own output,
--     one row per tic and one row per frame.
--
-- Fixed-point and angles follow the engine: a `fixed_t` is `Int32` in
-- 16.16, an `angle_t` is `UInt32` over the full turn, a `short` field is
-- `Int16`. A field that names another table's row holds that table's `id`,
-- and -1 means none wherever the engine writes -1.

CREATE DATABASE IF NOT EXISTS {{DB}};

-- ---------------------------------------------------------------------------
-- Raw input
-- ---------------------------------------------------------------------------

-- One row per WAD directory entry, bytes undecoded. `id` is the directory
-- position, which is the only unique key: a name repeats once per map, and
-- `doom1.wad` repeats one outside a map too. `map_marker` is the enclosing
-- `ExMy` marker, empty for a lump no map owns.
CREATE TABLE IF NOT EXISTS {{DB}}.wad_lumps
(
    id          UInt32,
    name        String,
    map_marker  String,
    bytes       String
)
ENGINE = MergeTree
ORDER BY id;

-- ---------------------------------------------------------------------------
-- The engine's constant tables. `native/tables/README.md` says which C array
-- each one is and how to regenerate it.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS {{DB}}.states
(
    id         UInt32,
    sprite     Int32,
    frame      Int32,   -- bit 15 is FF_FULLBRIGHT
    tics       Int32,
    action     UInt32,  -- action_functions.id, 0 for none
    nextstate  UInt32,
    misc1      Int32,
    misc2      Int32
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.action_functions
(
    id    UInt32,
    name  String
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.mobjinfo
(
    id            UInt32,
    doomednum     Int32,
    spawnstate    Int32,
    spawnhealth   Int32,
    seestate      Int32,
    seesound      Int32,
    reactiontime  Int32,
    attacksound   Int32,
    painstate     Int32,
    painchance    Int32,
    painsound     Int32,
    meleestate    Int32,
    missilestate  Int32,
    deathstate    Int32,
    xdeathstate   Int32,
    deathsound    Int32,
    speed         Int32,
    radius        Int32,
    height        Int32,
    mass          Int32,
    damage        Int32,
    activesound   Int32,
    flags         Int32,
    raisestate    Int32
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.sprnames
(
    id    UInt32,
    name  String
)
ENGINE = MergeTree ORDER BY id;

-- `sfxenum_t`, which is what `mobjinfo`'s five sound fields hold. `NUMSFX`
-- closes the enumerator list and is not a sound, so it is not a row.
CREATE TABLE IF NOT EXISTS {{DB}}.sfxenum
(
    id    UInt32,
    name  String
)
ENGINE = MergeTree ORDER BY id;

-- `mobjtype_t`, which is what `mobjinfo` is indexed by and what a routine
-- switching on a thing's kind names. `NUMMOBJTYPES` closes the enumerator
-- list and is not a type, so it is not a row.
CREATE TABLE IF NOT EXISTS {{DB}}.mobjtype
(
    id    UInt32,
    name  String
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.weaponinfo
(
    id          UInt32,
    ammo        Int32,
    upstate     Int32,
    downstate   Int32,
    readystate  Int32,
    atkstate    Int32,
    flashstate  Int32
)
ENGINE = MergeTree ORDER BY id;

-- The last row is the terminator `P_InitPicAnims` stops at: istexture = -1.
CREATE TABLE IF NOT EXISTS {{DB}}.animdefs
(
    id         UInt32,
    istexture  Int32,
    endname    String,
    startname  String,
    speed      Int32
)
ENGINE = MergeTree ORDER BY id;

-- The last row is the terminator `P_InitSwitchList` stops at: an empty
-- name1.
CREATE TABLE IF NOT EXISTS {{DB}}.switchlist
(
    id       UInt32,
    name1    String,
    name2    String,
    episode  Int32
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.checkcoord
(
    id  UInt32,
    c0  Int32,
    c1  Int32,
    c2  Int32,
    c3  Int32
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.finetangent (id UInt32, value Int32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.finesine (id UInt32, value Int32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.tantoangle (id UInt32, value UInt32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.rndtable (id UInt32, value UInt8)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.fuzzoffset (id UInt32, value Int32)
ENGINE = MergeTree ORDER BY id;

-- `P_NewChaseDir`'s own tables: the direction opposite each one, the four
-- diagonals it picks a direct route from, and how far a step of speed 1
-- carries along each direction.
CREATE TABLE IF NOT EXISTS {{DB}}.opposite (id UInt32, value UInt32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.diags (id UInt32, value UInt32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.xspeed (id UInt32, value Int32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.yspeed (id UInt32, value Int32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.gammatable (level UInt8, id UInt32, value UInt8)
ENGINE = MergeTree ORDER BY (level, id);

-- Every string `d_englsh.h` defines. The state row carries the message a
-- player is shown as the xxHash64 of its bytes, and the renderer takes the
-- row whose text hashes to that.
CREATE TABLE IF NOT EXISTS {{DB}}.messages (name String, text String)
ENGINE = MergeTree ORDER BY name;

-- ---------------------------------------------------------------------------
-- Level geometry, decoded from the map lumps by `native/sql/level_load.sql`.
-- Static: what `P_SetupLevel` builds once. What a tic changes lives in
-- `native_state`.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS {{DB}}.lv_vertexes
(
    id  UInt32,
    x   Int32,
    y   Int32
)
ENGINE = MergeTree ORDER BY id;

-- `dx`, `dy`, `slopetype` and `bbox` are what `P_LoadLineDefs` derives from
-- the two vertices. `side1` and `sector1` are -1 on a one-sided line.
CREATE TABLE IF NOT EXISTS {{DB}}.lv_lines
(
    id         UInt32,
    v1         UInt32,
    v2         UInt32,
    dx         Int32,
    dy         Int32,
    flags      Int16,
    special    Int16,
    tag        Int16,
    slopetype  UInt8,
    bbox       Array(Int32),  -- top, bottom, left, right
    side0      Int32,
    side1      Int32,
    sector0    Int32,
    sector1    Int32
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.lv_sides
(
    id             UInt32,
    textureoffset  Int32,
    rowoffset      Int32,
    toptexture     Int16,
    bottomtexture  Int16,
    midtexture     Int16,
    sector         UInt32
)
ENGINE = MergeTree ORDER BY id;

-- `blockbox`, `soundorg_x`, `soundorg_y` and `lines` are what
-- `P_GroupLines` derives. `lines` holds the id of every line touching the
-- sector, in line order.
CREATE TABLE IF NOT EXISTS {{DB}}.lv_sectors_static
(
    id             UInt32,
    floorheight    Int32,
    ceilingheight  Int32,
    floorpic       Int16,
    ceilingpic     Int16,
    lightlevel     Int16,
    special        Int16,
    tag            Int16,
    blockbox       Array(Int32),
    soundorg_x     Int32,
    soundorg_y     Int32,
    lines          Array(UInt32)
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.lv_segs
(
    id           UInt32,
    v1           UInt32,
    v2           UInt32,
    offset       Int32,
    angle        UInt32,
    sidedef      UInt32,
    linedef      UInt32,
    frontsector  Int32,
    backsector   Int32
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.lv_subsectors
(
    id         UInt32,
    numlines   UInt32,
    firstline  UInt32,
    sector     UInt32
)
ENGINE = MergeTree ORDER BY id;

-- `children` holds the lump's own two values, bit 15 still set for a
-- subsector. `bbox` is the two children's boxes end to end: right's four,
-- then left's four, each top, bottom, left, right.
CREATE TABLE IF NOT EXISTS {{DB}}.lv_nodes
(
    id        UInt32,
    x         Int32,
    y         Int32,
    dx        Int32,
    dy        Int32,
    bbox      Array(Int32),
    children  Array(UInt16)
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.lv_things
(
    id       UInt32,
    x        Int16,
    y        Int16,
    angle    Int16,
    type     Int16,
    options  Int16
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.lv_blockmap_header
(
    origin_x  Int32,
    origin_y  Int32,
    columns   UInt32,
    rows      UInt32
)
ENGINE = MergeTree ORDER BY origin_x;

-- One row per blockmap cell, `cell` = by * columns + bx. `lines` holds the
-- cell's line list in lump order, which `P_BlockLinesIterator` walks in
-- that order.
CREATE TABLE IF NOT EXISTS {{DB}}.lv_blockmap
(
    cell   UInt32,
    bx     UInt32,
    by     UInt32,
    lines  Array(UInt16)
)
ENGINE = MergeTree ORDER BY cell;

-- `P_LoadReject` reads REJECT as `numsectors * numsectors` bits packed end
-- to end, padded out when the lump is short. One row, holding the padded
-- matrix: pair (s1, s2) is bit `s1 * numsectors + s2`.
CREATE TABLE IF NOT EXISTS {{DB}}.lv_reject
(
    id     UInt8,
    bits   String
)
ENGINE = MergeTree ORDER BY id;

-- ---------------------------------------------------------------------------
-- Derived geometry: what a traversal would recompute every tic.
-- ---------------------------------------------------------------------------

-- The path from the BSP root down to each subsector. `nodes` holds the
-- node ids in order, `sides` the branch taken at each: 0 right, 1 left.
CREATE TABLE IF NOT EXISTS {{DB}}.lv_ssec_path
(
    subsector  UInt32,
    depth      UInt32,
    nodes      Array(UInt32),
    sides      Array(UInt8)
)
ENGINE = MergeTree ORDER BY subsector;

CREATE TABLE IF NOT EXISTS {{DB}}.lv_sector_subsectors
(
    sector      UInt32,
    subsectors  Array(UInt32)
)
ENGINE = MergeTree ORDER BY sector;

-- The subsectors under each node, as the closed range of subsector ids the
-- node's subtree covers. Subsector ids ascend with the BSP build order, so
-- a subtree is contiguous.
CREATE TABLE IF NOT EXISTS {{DB}}.lv_node_range
(
    node        UInt32,
    first_ssec  UInt32,
    last_ssec   UInt32
)
ENGINE = MergeTree ORDER BY node;

-- ---------------------------------------------------------------------------
-- Textures, composed from PNAMES and TEXTURE1.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS {{DB}}.pnames
(
    id    UInt32,
    name  String,
    lump  Int32   -- wad_lumps.id, -1 when the WAD has no such lump
)
ENGINE = MergeTree ORDER BY id;

-- Every lump in `patch_t` form: the texture patches, the sprites and the
-- status bar and font graphics. `columnofs` is the lump's own offset table,
-- one per column.
CREATE TABLE IF NOT EXISTS {{DB}}.patch_lumps
(
    lump        UInt32,   -- wad_lumps.id
    name        String,
    width       UInt16,
    height      UInt16,
    leftoffset  Int16,
    topoffset   Int16,
    columnofs   Array(UInt32)
)
ENGINE = MergeTree ORDER BY lump;

-- Every post of every patch column, in the order the column chains them.
-- `ofs` is the byte offset of the post's pixels inside the lump, which is
-- the post's own start plus the three bytes of header and pad.
CREATE TABLE IF NOT EXISTS {{DB}}.patch_posts
(
    lump      UInt32,
    col       UInt16,
    idx       UInt16,
    topdelta  UInt8,
    length    UInt8,
    ofs       UInt32
)
ENGINE = MergeTree ORDER BY (lump, col, idx);

-- `widthmask` is `R_InitTextures`'s `texturewidthmask`: the largest power
-- of two not past `width`, minus one. `height_fixed` is `height << 16`.
CREATE TABLE IF NOT EXISTS {{DB}}.tex_textures
(
    id            UInt32,
    name          String,
    width         UInt16,
    height        UInt16,
    widthmask     UInt16,
    height_fixed  Int32,
    masked        UInt8,
    patchcount    UInt16
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.tex_patches
(
    texture  UInt32,
    idx      UInt16,
    originx  Int16,
    originy  Int16,
    patch    UInt32,  -- pnames.id
    lump     Int32    -- wad_lumps.id of that patch
)
ENGINE = MergeTree ORDER BY (texture, idx);

-- Which patches cover each texture column, in patch order. `patch_col` is
-- the column of the patch that lands on the texture column, and `originy`
-- is where the patch's rows start in the texture.
CREATE TABLE IF NOT EXISTS {{DB}}.tex_col_patches
(
    texture    UInt32,
    col        UInt16,
    idx        UInt16,
    lump       UInt32,
    patch_col  UInt16,
    originy    Int16
)
ENGINE = MergeTree ORDER BY (texture, col, idx);

-- `R_GenerateLookup`'s per-column result. `lump` is the single patch's
-- lump when one patch covers the column, -1 when the column is composed.
-- `ofs` is the byte offset of the column's posts inside that lump, or
-- inside the texture's composite when `lump` is -1.
-- `uncovered` is 1 when no post reaches some row of a composed column, so
-- the composite keeps the zero it was built from there. `R_GenerateComposite`
-- leaves those rows at whatever the allocation held.
CREATE TABLE IF NOT EXISTS {{DB}}.tex_columns
(
    texture    UInt32,
    col        UInt16,
    patches    UInt16,
    lump       Int32,
    ofs        UInt32,
    uncovered  UInt8
)
ENGINE = MergeTree ORDER BY (texture, col);

-- `R_GenerateComposite`'s buffer, for a texture with more than one patch on
-- some column. Its length is `texturecompositesize`.
CREATE TABLE IF NOT EXISTS {{DB}}.tex_composite
(
    texture  UInt32,
    data     String
)
ENGINE = MergeTree ORDER BY texture;

-- The first byte of each texture's columns in `tex_window`, so a column's
-- window is at `tex_col_base[texture] + col`.
CREATE TABLE IF NOT EXISTS {{DB}}.tex_col_base
(
    texture  UInt32,
    base     UInt32
)
ENGINE = MergeTree ORDER BY texture;

-- One 128-byte window per texture column, zero padded. `overrun` is 1 when
-- the column's source held fewer than 128 bytes from `ofs`, so a draw that
-- reads past the end reads padding rather than the next column.
CREATE TABLE IF NOT EXISTS {{DB}}.tex_window
(
    slot     UInt32,
    texture  UInt32,
    col      UInt16,
    window   String,
    overrun  UInt8
)
ENGINE = MergeTree ORDER BY slot;

-- The posts of each column of a masked texture, which a mid texture draws
-- one at a time rather than as a solid column.
CREATE TABLE IF NOT EXISTS {{DB}}.tex_posts
(
    texture   UInt32,
    col       UInt16,
    idx       UInt16,
    topdelta  UInt8,
    length    UInt8,
    data      String
)
ENGINE = MergeTree ORDER BY (texture, col, idx);

-- ---------------------------------------------------------------------------
-- Sprites, flats and the rest of the assets.
-- ---------------------------------------------------------------------------

-- `R_InitSpriteDefs`'s result. `lump` and `flip` are eight long, one per
-- rotation; `rotate` is 0 when one frame serves every rotation.
CREATE TABLE IF NOT EXISTS {{DB}}.sprite_frames
(
    sprite  UInt32,   -- sprnames.id
    frame   UInt8,
    rotate  UInt8,
    lump    Array(Int32),
    flip    Array(UInt8)
)
ENGINE = MergeTree ORDER BY (sprite, frame);

-- `R_InitSpriteLumps`'s tables. `id` is the engine's own sprite lump
-- number, counted from the lump after `S_START`, which is what
-- `sprite_frames.lump` and `sprite_posts.lump` hold. The three scaled
-- fields are the `fixed_t` forms `R_ProjectSprite` reads.
CREATE TABLE IF NOT EXISTS {{DB}}.sprite_lumps
(
    id           UInt32,
    lump         UInt32,   -- wad_lumps.id
    name         String,
    width        UInt16,
    height       UInt16,
    width_fixed  Int32,
    leftoffset   Int32,
    topoffset    Int32
)
ENGINE = MergeTree ORDER BY id;

-- Every sprite column's pixels, end to end. `sprite_posts.pool_ofs` points
-- into this.
CREATE TABLE IF NOT EXISTS {{DB}}.sprite_pool
(
    id    UInt8,
    data  String
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.sprite_posts
(
    lump      UInt32,   -- sprite_lumps.id
    col       UInt16,
    idx       UInt16,
    topdelta  UInt8,
    length    UInt8,
    pool_ofs  UInt32
)
ENGINE = MergeTree ORDER BY (lump, col, idx);

CREATE TABLE IF NOT EXISTS {{DB}}.flats
(
    id    UInt32,
    name  String,
    data  String   -- 4,096 bytes, 64x64
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.colormap
(
    id    UInt8,
    data  String   -- 256 bytes
)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.playpal
(
    id    UInt8,
    data  String   -- 768 bytes, 256 RGB triples
)
ENGINE = MergeTree ORDER BY id;

-- The status bar, font and menu patches, by lump name.
CREATE TABLE IF NOT EXISTS {{DB}}.ui_patches
(
    id          UInt32,   -- wad_lumps.id
    name        String,
    width       UInt16,
    height      UInt16,
    leftoffset  Int16,
    topoffset   Int16,
    columnofs   Array(UInt32),
    data        String
)
ENGINE = MergeTree ORDER BY id;

-- ---------------------------------------------------------------------------
-- Demo input
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS {{DB}}.demo_header
(
    id        UInt8,
    version   UInt8,
    skill     UInt8,
    episode   UInt8,
    map       UInt8,
    deathmatch UInt8,
    respawn   UInt8,
    fast      UInt8,
    nomonsters UInt8,
    consoleplayer UInt8
)
ENGINE = MergeTree ORDER BY id;

-- One tic command per row, `tic` counted from 1: the demo's first command
-- drives the first tic.
CREATE TABLE IF NOT EXISTS {{DB}}.demo_cmds
(
    tic          UInt32,
    forwardmove  Int8,
    sidemove     Int8,
    angleturn    Int16,
    buttons      UInt8
)
ENGINE = MergeTree ORDER BY tic;

-- ---------------------------------------------------------------------------
-- The simulation's output.
--
-- Join engines, because every tic reads the tic before it and every frame
-- reads the frame before it. `joinGet` on the key is a hash lookup against
-- a table held in memory, and the resident INSERT does one per row.
-- `join_any_take_last_row` so re-running a tic replaces its row instead of
-- keeping the first.
--
-- The column list and its order are `spec/src/native_state.rs`, which the
-- reference emulator's probe writes as well. Mobj and sector-thinker
-- fields are parallel arrays indexed by slot in thinker-list order.
-- `p_message` and `hu_message` hold the xxh64 of the text, 0 for none,
-- because that is what both writers can read out of a message.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS {{DB}}.native_state
(
    tic            UInt32,

    -- Game
    leveltime      Int32,
    prndindex      UInt8,
    rndindex       UInt8,
    next_seq       UInt32,
    next_linkseq   UInt32,
    paused         UInt8,
    demo_end       UInt8,
    totalkills     Int32,
    totalitems     Int32,
    totalsecret    Int32,

    -- Mobjs
    m_id           Array(UInt32),
    m_x            Array(Int32),
    m_y            Array(Int32),
    m_z            Array(Int32),
    m_angle        Array(UInt32),
    m_sprite       Array(Int32),
    m_frame        Array(Int32),
    m_floorz       Array(Int32),
    m_ceilingz     Array(Int32),
    m_radius       Array(Int32),
    m_height       Array(Int32),
    m_momx         Array(Int32),
    m_momy         Array(Int32),
    m_momz         Array(Int32),
    m_type         Array(Int32),
    m_tics         Array(Int32),
    m_state        Array(Int32),
    m_flags        Array(Int32),
    m_health       Array(Int32),
    m_movedir      Array(Int32),
    m_movecount    Array(Int32),
    m_target       Array(UInt32),
    m_reactiontime Array(Int32),
    m_threshold    Array(Int32),
    m_player       Array(Int8),
    m_lastlook     Array(Int32),
    m_sp_x         Array(Int16),
    m_sp_y         Array(Int16),
    m_sp_angle     Array(Int16),
    m_sp_type      Array(Int16),
    m_sp_options   Array(Int16),
    m_tracer       Array(UInt32),
    m_subsector    Array(Int32),
    m_linkseq      Array(UInt32),

    -- Sector thinkers
    s_seq              Array(UInt32),
    s_kind             Array(UInt8),
    s_sector           Array(Int32),
    s_type             Array(Int32),
    s_direction        Array(Int32),
    s_speed            Array(Int32),
    s_dest             Array(Int32),
    s_dest2            Array(Int32),
    s_count            Array(Int32),
    s_wait             Array(Int32),
    s_status           Array(Int32),
    s_oldstatus        Array(Int32),
    s_crush            Array(UInt8),
    s_tag              Array(Int32),
    s_texture          Array(Int32),
    s_newspecial       Array(Int32),
    s_minlight         Array(Int32),
    s_maxlight         Array(Int32),
    s_mintime          Array(Int32),
    s_maxtime          Array(Int32),
    s_active           Array(UInt8),
    s_activeplat_slot  Array(Int32),
    s_activeceil_slot  Array(Int32),

    -- Sectors, indexed by sector number
    sec_floorheight    Array(Int32),
    sec_ceilingheight  Array(Int32),
    sec_floorpic       Array(Int16),
    sec_lightlevel     Array(Int16),
    sec_special        Array(Int16),
    sec_specialdata    Array(UInt32),
    sec_soundtarget    Array(UInt32),
    sec_soundtraversed Array(Int32),

    -- Lines and sides
    line_special        Array(Int16),
    side_toptexture     Array(Int16),
    side_midtexture     Array(Int16),
    side_bottomtexture  Array(Int16),
    side_textureoffset  Array(Int32),

    -- Switch buttons
    btn_line     Array(Int32),
    btn_where    Array(UInt8),
    btn_texture  Array(Int32),
    btn_timer    Array(Int32),

    -- Animation, indexed by picture number
    texturetranslation  Array(Int32),
    flattranslation     Array(Int32),

    -- Player one
    p_mo               UInt32,
    p_playerstate      UInt8,
    p_cmd_forwardmove  Int8,
    p_cmd_sidemove     Int8,
    p_cmd_angleturn    Int16,
    p_cmd_buttons      UInt8,
    p_viewz            Int32,
    p_viewheight       Int32,
    p_deltaviewheight  Int32,
    p_bob              Int32,
    p_health           Int32,
    p_armorpoints      Int32,
    p_armortype        Int32,
    p_powers           Array(Int32),
    p_cards            Array(UInt8),
    p_backpack         UInt8,
    p_readyweapon      Int32,
    p_pendingweapon    Int32,
    p_weaponowned      Array(Int32),
    p_ammo             Array(Int32),
    p_maxammo          Array(Int32),
    p_attackdown       UInt8,
    p_usedown          UInt8,
    p_cheats           Int32,
    p_refire           Int32,
    p_killcount        Int32,
    p_itemcount        Int32,
    p_secretcount      Int32,
    p_message          UInt64,
    p_damagecount      Int32,
    p_bonuscount       Int32,
    p_attacker         UInt32,
    p_extralight       Int32,
    p_fixedcolormap    Int32,

    -- The two player sprites
    psp_state  Array(Int32),
    psp_tics   Array(Int32),
    psp_sx     Array(Int32),
    psp_sy     Array(Int32),

    -- Status bar, heads-up display and menu
    st_faceindex        Int32,
    st_facecount        Int32,
    st_priority         Int32,
    st_lastattackdown   Int32,
    st_oldweaponsowned  Array(Int32),
    st_oldhealth        Int32,
    st_randomnumber     Int32,
    st_lastcalc         Int32,
    st_calc_oldhealth   Int32,
    st_palette          Int32,
    st_clock            Int32,
    hu_message_on       UInt8,
    hu_message_counter  Int32,
    hu_message          UInt64,
    hu_nottobefuckedwith UInt8,
    menu_skullanim      Int32,
    menu_whichskull     Int32,

    -- Interactive input carry
    turnheld  Int32,

    -- A tic the simulation could not produce in full. `unresolved` is 1
    -- when any field is a placeholder; `unimplemented` names which paths
    -- the tic reached but did not run, one bit each. Both are zero on a
    -- tic that ran completely.
    unresolved     UInt8,
    unimplemented  UInt64,

    -- What the tic drew from the random tables, in draw order: `dbg_ran`
    -- the call sites, `dbg_prnd` the values. A divergence in the draw
    -- sequence is what tells a wrong branch from a wrong arithmetic.
    dbg_ran   Array(UInt32),
    dbg_prnd  Array(UInt8)
)
ENGINE = Join(ANY, LEFT, tic)
SETTINGS join_any_take_last_row = 1;

-- One row per frame. `fb` is the 64,000 bytes the renderer drew, `fb_bytes`
-- the same as an array for a query that indexes it, `rgb32` the palette
-- applied. `fb_hash` is `spec::fb_hash` over the framebuffer and palette.
-- `fuzzpos` and `st_cache` persist between frames the way the engine's own
-- statics do: `fuzzpos` is `r_draw.c`'s, and `st_cache` is the last value
-- each status bar widget drew, so a frame redraws only what changed.
CREATE TABLE IF NOT EXISTS {{DB}}.native_frames
(
    frame          UInt32,
    tic            UInt32,
    fb             String,
    fb_bytes       Array(UInt8),
    palette        String,
    palette_index  UInt8,
    rgb32          String,
    fb_hash        UInt64,
    fuzzpos        UInt8,
    st_cache       Tuple(
        ready     Int32,
        frags     Int32,
        health    Int32,
        armor     Int32,
        ammo      Array(Int32),
        maxammo   Array(Int32),
        arms      Array(Int32),
        keyboxes  Array(Int32),
        faceindex Int32,
        armsbg    Int32
    )
)
ENGINE = Join(ANY, LEFT, frame)
SETTINGS join_any_take_last_row = 1;

-- How far the screen melt has advanced at each frame it covers. `passes` is
-- how many times `wipe_ScreenWipe` ran at that frame, and `melt_step` is the
-- running total the renderer takes. `driver/melt/` holds the rows the driver
-- streams in and says where they came from. A frame with no row here is not
-- a melt frame.
CREATE TABLE IF NOT EXISTS {{DB}}.melt_schedule
(
    frame      UInt32,
    passes     UInt8,
    melt_step  UInt8
)
ENGINE = MergeTree ORDER BY frame;

-- ---------------------------------------------------------------------------
-- The renderer's own tables, built by `native/sql/render_load.sql` from the
-- engine's constant tables and the level. They hold what
-- `R_ExecuteSetViewSize`, `R_InitTextureMapping` and `R_InitLightTables`
-- compute once for a 320x168 view with `screenblocks = 10`.
-- ---------------------------------------------------------------------------

-- `viewangletox`: the screen column a fine angle maps to. `x_raw` still
-- carries the two out-of-view markers, -1 and 321, which is what the
-- `xtoviewangle` scan reads; `x` has them pulled onto the edges of the view.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_viewangletox (id UInt32, x_raw Int32, x Int32)
ENGINE = MergeTree ORDER BY id;

-- `xtoviewangle`: the smallest view angle that maps to column `id`, one row
-- per column plus the fencepost at 320.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_xtoviewangle (id UInt32, angle UInt32)
ENGINE = MergeTree ORDER BY id;

-- `yslope`, one row per view row, and `distscale`, one per column.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_yslope (id UInt32, value Int32)
ENGINE = MergeTree ORDER BY id;

CREATE TABLE IF NOT EXISTS {{DB}}.rt_distscale (id UInt32, value Int32)
ENGINE = MergeTree ORDER BY id;

-- `scalelight` and `zlight` as the colormap they select, 0 to 31, rather
-- than as the pointer the engine holds.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_scalelight (light UInt8, scale UInt8, level UInt8)
ENGINE = MergeTree ORDER BY (light, scale);

CREATE TABLE IF NOT EXISTS {{DB}}.rt_zlight (light UInt8, z UInt8, level UInt8)
ENGINE = MergeTree ORDER BY (light, z);

-- The one row the sky needs: which flat number means sky, which texture the
-- episode's sky is, and `skytexturemid`.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_sky
(
    id          UInt8,
    flatnum     Int32,
    texture     Int32,
    texturemid  Int32
)
ENGINE = MergeTree ORDER BY id;

-- Which subsector each seg belongs to. `R_Subsector` reads the front sector
-- off the subsector, not off the seg.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_seg_subsector (seg UInt32, subsector UInt32)
ENGINE = MergeTree ORDER BY seg;

-- The pixel pools, one row each. A per-pixel lookup lands in one of these,
-- and each is a `WITH` constant in the frame transform, so a pool is read
-- once per statement rather than once per frame.

-- Every texture column's 128-byte window, end to end in slot order. A
-- column's bytes start at `(tex_col_base[texture] + col) * 128`.
--
-- A pool is a String and not an Array. Both work, but a query holds an array
-- constant as one field per element, and carrying a few hundred thousand of
-- those through the frame statement costs gigabytes; a String is one field
-- whatever its length.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_tex_pool (id UInt8, data String)
ENGINE = MergeTree ORDER BY id;

-- Every flat's 4,096 bytes, end to end in flat number order.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_flat_pool (id UInt8, data String)
ENGINE = MergeTree ORDER BY id;

-- COLORMAP end to end: light level `l` maps colour `c` to `data[l * 256 + c]`.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_colormap_pool (id UInt8, data String)
ENGINE = MergeTree ORDER BY id;

-- One row per PLAYPAL palette, with the gamma table already applied, which
-- is what the ROM writes to the palette register. `rgb` is the same palette
-- as 256 four-byte words, in the order a frame's pixels index it.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_palette (id UInt8, data String, rgb String)
ENGINE = MergeTree ORDER BY id;

-- The reference emulator's own frame, loaded beside ours so the two can be
-- compared in SQL. `native::sql::parity` builds the queries that read it.
CREATE TABLE IF NOT EXISTS {{DB}}.ref_frames
(
    frame    UInt32,
    fb       String,
    palette  String
)
ENGINE = MergeTree ORDER BY frame;

-- ---------------------------------------------------------------------------
-- The sprite side of the renderer, built by `native/sql/render_load.sql`.
-- ---------------------------------------------------------------------------

-- Every sprite lump's bytes, in sprite number order and spaced the way the
-- engine's zone allocator spaces the cached lumps. A draw reads the lump
-- itself rather than a repacked pool, so a column that reads past a post's
-- length reads what the engine reads.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_sprite_pool (id UInt8, data String)
ENGINE = MergeTree ORDER BY id;

-- Where each sprite lump starts in that pool, with the three scaled fields
-- `R_ProjectSprite` reads.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_sprite_lump
(
    id           UInt32,   -- sprite_lumps.id
    base         UInt32,
    width        UInt16,
    width_fixed  Int32,
    leftoffset   Int32,
    topoffset    Int32
)
ENGINE = MergeTree ORDER BY id;

-- `spriteframe_t` flattened to one row per rotation, so a frame's picture is
-- one lookup. `slot` is `(sprite * 32 + frame) * 8 + rotation`, and `lump` is
-- -1 for a rotation no picture serves.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_sprite_frame
(
    slot    UInt32,
    rotate  UInt8,
    lump    Int32,
    flip    UInt8
)
ENGINE = MergeTree ORDER BY slot;

-- Where each sprite column's posts sit in `rt_sprite_post`. `slot` is
-- `sprite lump * 256 + column`.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_sprite_colposts
(
    slot   UInt32,
    first  UInt32,
    num    UInt16
)
ENGINE = MergeTree ORDER BY slot;

-- Every sprite post, in column order. `ofs` is where its pixels sit in
-- `rt_sprite_pool`.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_sprite_post
(
    id        UInt32,
    topdelta  UInt8,
    length    UInt8,
    ofs       UInt32
)
ENGINE = MergeTree ORDER BY id;

-- Every message under the hash the state row names it by, which is what the
-- frame transform looks a message up with.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_message (hash UInt64, name String, text String)
ENGINE = MergeTree ORDER BY hash;

-- ---------------------------------------------------------------------------
-- The status bar and heads-up graphics, built by `native/sql/render_load.sql`.
-- ---------------------------------------------------------------------------

-- Every status bar, font and menu patch's bytes, end to end in the order
-- `rt_ui_patch` numbers them.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_ui_pool (id UInt8, data String)
ENGINE = MergeTree ORDER BY id;

-- One row per patch, numbered densely by name, with where its bytes start.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_ui_patch
(
    id          UInt32,
    name        String,
    base        UInt32,
    width       UInt16,
    height      UInt16,
    leftoffset  Int16,
    topoffset   Int16
)
ENGINE = MergeTree ORDER BY id;

-- Which patch each thing the engine draws is. `slot` is the numbering
-- `native/sql/render_load.sql` lays out and the frame transform indexes.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_ui_slot (slot UInt32, name String, patch UInt32)
ENGINE = MergeTree ORDER BY slot;

-- Where each patch column's posts sit in `rt_ui_post`. `slot` is
-- `patch * 512 + column`.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_ui_colposts (slot UInt32, first UInt32, num UInt16)
ENGINE = MergeTree ORDER BY slot;

CREATE TABLE IF NOT EXISTS {{DB}}.rt_ui_post
(
    id        UInt32,
    topdelta  UInt8,
    length    UInt8,
    ofs       UInt32
)
ENGINE = MergeTree ORDER BY id;

-- `st_backing_screen`: the status bar with nothing on it, 320 by 32, which
-- every widget copies back over its own area before it draws.
CREATE TABLE IF NOT EXISTS {{DB}}.rt_ui_backing (id UInt8, data String)
ENGINE = MergeTree ORDER BY id;
