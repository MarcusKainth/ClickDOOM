//! The frame transform: DOOM's renderer as one SQL expression.
//!
//! [`frame_transform`] builds the statement the resident render pipeline
//! runs. One input row `(frame, tic, melt_step)` produces one row of
//! `native_frames` from the state row for `tic` and the frame before it.
//!
//! The statement is one `INSERT ... SELECT` over a long `WITH` list. Each
//! binding is one stage, and the stages are in the order `R_RenderPlayerView`
//! runs them: view setup, the BSP walk, seg projection and clipping, the
//! wall loop, the visplanes, then the pixels. A stage reads the bindings
//! before it and nothing after it.
//!
//! Two rules shape every expression here, both measured on the pinned
//! server. A lambda that captures a per-frame array pays for one copy of
//! that array per element of the array it maps over, so a stage's cost is
//! the product of the two lengths and per-pixel random access is only ever
//! into a `WITH` constant. And nothing blocks: no `GROUP BY`, no `ORDER BY`,
//! no window function, because the statement stays open across frames.

use crate::sql::fixed;

/// The view, fixed by `screenblocks = 10`.
const VIEW_WIDTH: i32 = 320;
const VIEW_HEIGHT: i32 = 168;
const SCREEN_HEIGHT: i32 = 200;
const CENTER_X: i32 = 160;
const CENTER_Y: i32 = 84;
/// `centerxfrac`, which is also `projection`.
const CENTER_X_FRAC: i32 = CENTER_X << 16;
/// `centeryfrac >> 4`, which is what the wall loop steps in.
const CENTER_Y_FRAC_4: i32 = (CENTER_Y << 16) >> 4;

/// `MAXDRAWSEGS`. A fragment past this one is clipped away but not drawn.
const MAX_DRAWSEGS: usize = 256;

/// When the heads-up message is drawn: after the view and everything in it.
const MESSAGE_TIME: u32 = 1_048_500;

/// When the status bar is drawn. It never shares a pixel with the view, so
/// where it sits relative to the rest only has to be the same every frame.
const STATUS_TIME: u32 = 1_040_000;

/// The first row of the status bar.
const STATUS_BAR_Y: i32 = VIEW_HEIGHT;

/// The pixels of the framebuffer, and of the view inside it.
const FB_BYTES: i32 = VIEW_WIDTH * SCREEN_HEIGHT;

/// One `WITH` binding: a name and the expression behind it.
struct Stage {
    bindings: Vec<(String, String)>,
    db: String,
}

impl Stage {
    fn new(db: &str) -> Stage {
        Stage {
            bindings: Vec::new(),
            db: db.to_owned(),
        }
    }

    fn bind(&mut self, name: &str, expr: impl Into<String>) {
        self.bindings.push((name.to_owned(), expr.into()));
    }

    /// A whole column of a table as one array constant, in `order`.
    ///
    /// The order is carried into the array and sorted there. `groupArray`
    /// does not promise the order it collects rows in, and a subquery's own
    /// `ORDER BY` does not bind it, so a table read in parallel comes out
    /// shuffled.
    fn table(&mut self, name: &str, table: &str, column: &str, order: &str) {
        let db = &self.db;
        self.bind(
            name,
            format!(
                "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, \
                 groupArray(({order}, {column})))) FROM {db}.{table})"
            ),
        );
    }

    /// A per-tic column of the state row.
    fn state(&mut self, name: &str, column: &str) {
        let db = &self.db;
        self.bind(
            name,
            format!("joinGet('{db}.native_state', '{column}', toUInt32(tic))"),
        );
    }

    /// The stages as nested subqueries, innermost first.
    ///
    /// A stage cannot be a `WITH` alias: the analyser substitutes an alias
    /// wherever it is named, so a chain of stages that each read the one
    /// before them grows the query tree by a factor per stage. A column of a
    /// subquery is named once and read as a column, so the tree stays the
    /// size of the text. Each stage goes into the first layer past every
    /// stage it reads, and a layer carries the layers under it forward with
    /// `*`.
    fn text(&self, source: &str) -> String {
        let mut level = Vec::with_capacity(self.bindings.len());
        for (at, (_, expr)) in self.bindings.iter().enumerate() {
            let deep = self.bindings[..at]
                .iter()
                .enumerate()
                .filter(|(_, (name, _))| names(expr).any(|word| word == name.as_str()))
                .map(|(under, _)| level[under] + 1)
                .max();
            level.push(deep.unwrap_or(0));
        }
        let depth = level.iter().max().map_or(0, |top| top + 1);
        let mut sql = format!("(SELECT frame, tic, melt_step FROM {source})");
        for layer in 0..depth {
            let here = self
                .bindings
                .iter()
                .zip(&level)
                .filter(|(_, at)| **at == layer)
                .map(|((name, expr), _)| format!("        {expr} AS {name}"))
                .collect::<Vec<_>>()
                .join(",\n");
            sql = format!("(\n    SELECT *,\n{here}\n    FROM {sql}\n)");
        }
        sql
    }
}

/// The identifiers in an expression, so a stage can be told which stages it
/// reads.
fn names(expr: &str) -> impl Iterator<Item = &str> {
    expr.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
}

/// The statement the resident render pipeline runs.
///
/// The input row is `(frame, tic, melt_step, pad)`. The server reads
/// `max_query_size` bytes before it parses, so the first row of the body is
/// padding and carries the bytes that make up the difference in `pad`. Every
/// real row leaves `pad` empty, and that is what tells the two apart: frame 0
/// is the melt's first frame and a filter on the frame number would throw it
/// away.
pub fn frame_transform(db: &str) -> String {
    let source = "input('frame UInt32, tic UInt32, melt_step UInt8, pad String') WHERE empty(pad)";
    statement(db, source)
}

/// The same transform over an arbitrary source of `(frame, tic, melt_step)`
/// rows, which is what a test drives it with. A plain `INSERT ... SELECT`
/// over one row computes what one streamed row computes.
pub fn frame_transform_over(db: &str, source: &str) -> String {
    statement(db, source)
}

fn statement(db: &str, source: &str) -> String {
    let mut s = Stage::new(db);
    constants(&mut s);
    view_setup(&mut s);
    bsp_order(&mut s);
    seg_project(&mut s);
    seg_classify(&mut s);
    solid_columns(&mut s);
    check_bbox(&mut s);
    fragments(&mut s);
    store_wall_range(&mut s);
    render_seg_loop(&mut s);
    visplanes(&mut s);
    sprites(&mut s);
    sprite_clip(&mut s);
    wall_pixels(&mut s);
    plane_pixels(&mut s);
    sky_pixels(&mut s);
    sprite_pixels(&mut s);
    psprites(&mut s);
    fuzz(&mut s);
    message(&mut s);
    status_bar(&mut s);
    compose(&mut s);
    format!(
        "INSERT INTO {db}.native_frames\nSELECT\n{}\nFROM {}",
        output_columns(),
        s.text(source)
    )
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Everything that does not change between frames, as scalar array
/// constants. A scalar subquery is evaluated once per statement, so a
/// resident statement pays for these when it is analysed and never again.
fn constants(s: &mut Stage) {
    let db = s.db.clone();

    s.table("k_finesine", "finesine", "value", "id");
    s.table("k_finetangent", "finetangent", "value", "id");
    s.table("k_tantoangle", "tantoangle", "value", "id");
    s.table("k_vatox", "rt_viewangletox", "x", "id");
    s.table("k_xtova", "rt_xtoviewangle", "angle", "id");
    s.table("k_yslope", "rt_yslope", "value", "id");
    s.table("k_distscale", "rt_distscale", "value", "id");
    s.table("k_scalelight", "rt_scalelight", "level", "(light, scale)");
    s.table("k_zlight", "rt_zlight", "level", "(light, z)");

    s.bind(
        "k_clipangle",
        format!("assumeNotNull((SELECT angle FROM {db}.rt_xtoviewangle WHERE id = 0))"),
    );
    s.bind(
        "k_skyflat",
        format!("assumeNotNull((SELECT flatnum FROM {db}.rt_sky WHERE id = 0))"),
    );
    s.bind(
        "k_skytex",
        format!("assumeNotNull((SELECT texture FROM {db}.rt_sky WHERE id = 0))"),
    );
    s.bind(
        "k_skymid",
        format!("assumeNotNull((SELECT texturemid FROM {db}.rt_sky WHERE id = 0))"),
    );

    // Pixel pools. Every per-pixel lookup lands in one of these. Each is a
    // string, because a query holds an array constant as one field per
    // element and carrying a few hundred thousand of those through the
    // statement costs gigabytes.
    s.bind(
        "k_texpool",
        format!("assumeNotNull((SELECT data FROM {db}.rt_tex_pool))"),
    );
    s.bind(
        "k_flatpool",
        format!("assumeNotNull((SELECT data FROM {db}.rt_flat_pool))"),
    );
    s.bind(
        "k_colormap",
        format!("assumeNotNull((SELECT data FROM {db}.rt_colormap_pool))"),
    );
    s.table("k_palettes", "rt_palette", "data", "id");
    s.table("k_rgb", "rt_palette", "rgb", "id");

    s.table("k_tex_base", "tex_col_base", "base", "texture");
    s.table("k_tex_mask", "tex_textures", "widthmask", "id");
    s.table("k_tex_height", "tex_textures", "height_fixed", "id");

    // Geometry. A seg's two vertices are resolved here so the frame does
    // not join them.
    for (name, expr) in [
        ("k_seg_v1x", "v1.x"),
        ("k_seg_v1y", "v1.y"),
        ("k_seg_v2x", "v2.x"),
        ("k_seg_v2y", "v2.y"),
    ] {
        s.bind(
            name,
            format!(
                "(SELECT groupArray(c) FROM (SELECT {expr} AS c FROM {db}.lv_segs AS sg \
                 INNER JOIN {db}.lv_vertexes AS v1 ON v1.id = sg.v1 \
                 INNER JOIN {db}.lv_vertexes AS v2 ON v2.id = sg.v2 ORDER BY sg.id))"
            ),
        );
    }
    s.table("k_seg_angle", "lv_segs", "angle", "id");
    s.table("k_seg_offset", "lv_segs", "offset", "id");
    s.table("k_seg_side", "lv_segs", "sidedef", "id");
    s.table("k_seg_line", "lv_segs", "linedef", "id");
    s.table("k_seg_back", "lv_segs", "backsector", "id");
    s.table("k_seg_ssec", "rt_seg_subsector", "subsector", "seg");

    s.table("k_line_flags", "lv_lines", "flags", "id");
    s.table("k_side_rowoffset", "lv_sides", "rowoffset", "id");

    s.table("k_ssec_first", "lv_subsectors", "firstline", "id");
    s.table("k_ssec_num", "lv_subsectors", "numlines", "id");
    s.table("k_ssec_sector", "lv_subsectors", "sector", "id");
    s.table("k_path_nodes", "lv_ssec_path", "nodes", "subsector");

    s.table("k_node_x", "lv_nodes", "x", "id");
    s.table("k_node_y", "lv_nodes", "y", "id");
    s.table("k_node_dx", "lv_nodes", "dx", "id");
    s.table("k_node_dy", "lv_nodes", "dy", "id");
    s.table("k_node_bbox", "lv_nodes", "bbox", "id");
    s.table("k_node_child", "lv_nodes", "children", "id");
    s.table("k_range_first", "lv_node_range", "first_ssec", "node");
    s.table("k_range_last", "lv_node_range", "last_ssec", "node");

    s.table("k_sec_ceilpic", "lv_sectors_static", "ceilingpic", "id");

    // Sprites. `rt_sprite_frame` is one row per rotation of every frame and
    // `rt_sprite_colposts` one per column of every picture, both padded to a
    // fixed stride so a lookup is one index.
    s.table("k_spr_rotate", "rt_sprite_frame", "rotate", "slot");
    s.table("k_spr_lump", "rt_sprite_frame", "lump", "slot");
    s.table("k_spr_flip", "rt_sprite_frame", "flip", "slot");
    s.table("k_spost_first", "rt_sprite_colposts", "first", "slot");
    s.table("k_spost_num", "rt_sprite_colposts", "num", "slot");
    s.table("k_spost_top", "rt_sprite_post", "topdelta", "id");
    s.table("k_spost_len", "rt_sprite_post", "length", "id");
    s.table("k_spost_ofs", "rt_sprite_post", "ofs", "id");
    s.table("k_spl_widthf", "rt_sprite_lump", "width_fixed", "id");
    s.table("k_spl_left", "rt_sprite_lump", "leftoffset", "id");
    s.table("k_spl_top", "rt_sprite_lump", "topoffset", "id");
    s.table("k_state_sprite", "states", "sprite", "id");
    s.table("k_state_frame", "states", "frame", "id");

    // The status bar and heads-up graphics, in the same shape as the sprite
    // pictures: a patch index, a post list per column, and one byte pool.
    s.table("k_ui_slot", "rt_ui_slot", "patch", "slot");
    s.table("k_ui_base", "rt_ui_patch", "base", "id");
    s.table("k_ui_width", "rt_ui_patch", "width", "id");
    s.table("k_ui_left", "rt_ui_patch", "leftoffset", "id");
    s.table("k_ui_top", "rt_ui_patch", "topoffset", "id");
    s.table("k_ui_height", "rt_ui_patch", "height", "id");
    s.table("k_weapon_ammo", "weaponinfo", "ammo", "id");
    s.bind(
        "k_ui_backing",
        format!("assumeNotNull((SELECT data FROM {db}.rt_ui_backing))"),
    );
    s.table("k_uipost_first", "rt_ui_colposts", "first", "slot");
    s.table("k_uipost_num", "rt_ui_colposts", "num", "slot");
    s.table("k_uipost_top", "rt_ui_post", "topdelta", "id");
    s.table("k_uipost_len", "rt_ui_post", "length", "id");
    s.table("k_uipost_ofs", "rt_ui_post", "ofs", "id");
    s.table("k_msg_hash", "rt_message", "hash", "hash");
    s.table("k_msg_text", "rt_message", "text", "hash");
    s.bind(
        "k_uipool",
        format!("assumeNotNull((SELECT data FROM {db}.rt_ui_pool))"),
    );
    s.bind(
        "k_sprpool",
        format!("assumeNotNull((SELECT data FROM {db}.rt_sprite_pool))"),
    );

    // `checkcoord` as one flat array of 48, four entries per box position.
    s.bind(
        "k_checkcoord",
        format!(
            "(SELECT groupArray(c) FROM (SELECT arrayJoin([c0, c1, c2, c3]) AS c \
             FROM {db}.checkcoord ORDER BY id))"
        ),
    );

    s.bind("k_nssec", "toUInt32(length(k_ssec_first))");
    s.bind("k_nnode", "toUInt32(length(k_node_x))");
    s.bind(
        "k_ssec_ids",
        "arrayMap(i -> toUInt32(i - 1), arrayEnumerate(k_ssec_first))",
    );

    // What each step of a subsector's path leads to: the next node down, and
    // the subsector itself at the end, in the form the lump writes a child.
    s.bind(
        "k_path_next",
        "arrayMap((ns, sc) -> arrayPushBack(arrayPopFront(arrayMap(n -> toUInt16(n), ns)), \
         toUInt16(sc + 32768)), k_path_nodes, k_ssec_ids)",
    );
    // The branch taken at each node of the path: the one whose child is the
    // step after it.
    s.bind(
        "k_path_sides",
        "arrayMap((ns, nx) -> arrayMap((n, c) -> toUInt8(if(k_node_child[n + 1][1] = c, 0, 1)), \
         ns, nx), k_path_nodes, k_path_next)",
    );
}

// ---------------------------------------------------------------------------
// R_SetupFrame
// ---------------------------------------------------------------------------

fn view_setup(s: &mut Stage) {
    s.state("st_mo", "p_mo");
    s.state("st_mx", "m_x");
    s.state("st_my", "m_y");
    s.state("st_mangle", "m_angle");
    s.state("st_secfloor", "sec_floorheight");
    s.state("st_secceil", "sec_ceilingheight");
    s.state("st_secfloorpic", "sec_floorpic");
    s.state("st_seclight", "sec_lightlevel");
    s.state("st_sidetop", "side_toptexture");
    s.state("st_sidemid", "side_midtexture");
    s.state("st_sidebot", "side_bottomtexture");
    s.state("st_sideoff", "side_textureoffset");
    s.state("st_texxlat", "texturetranslation");
    s.state("st_flatxlat", "flattranslation");

    s.state("v_z", "p_viewz");
    s.state("v_extralight", "p_extralight");
    s.state("v_fixedcolormap", "p_fixedcolormap");
    s.bind("v_x", "st_mx[st_mo]");
    s.bind("v_y", "st_my[st_mo]");
    s.bind("v_angle", "st_mangle[st_mo]");

    // `basexscale` and `baseyscale`, the two flat steps a span walks in.
    let cos_left = "k_finesine[bitShiftRight(toUInt32(4294967296 + toUInt64(v_angle) - 1073741824), 19) + 2049]";
    let sin_left =
        "k_finesine[bitShiftRight(toUInt32(4294967296 + toUInt64(v_angle) - 1073741824), 19) + 1]";
    s.bind(
        "v_basex",
        fixed::fixed_div(cos_left, &CENTER_X_FRAC.to_string()),
    );
    s.bind(
        "v_basey",
        format!(
            "toInt32(-toInt64({}))",
            fixed::fixed_div(sin_left, &CENTER_X_FRAC.to_string())
        ),
    );

    // The three side textures of a sidedef in one word, so a gather that
    // wants all three pays for one array rather than three.
    s.bind(
        "st_sidepack",
        "arrayMap((t, m, b) -> bitOr(bitOr(bitShiftLeft(toUInt64(toUInt16(t)), 32), \
         bitShiftLeft(toUInt64(toUInt16(m)), 16)), toUInt64(toUInt16(b))), \
         st_sidetop, st_sidemid, st_sidebot)",
    );
}

// ---------------------------------------------------------------------------
// R_RenderBSPNode, without the recursion
// ---------------------------------------------------------------------------

/// The pre-order the recursion would visit subsectors in.
///
/// `R_PointOnSide` at each node says which child the walk enters first. A
/// subsector's key takes one bit per ancestor, set when the subsector is on
/// the far side of that ancestor, most significant bit at the root. Sorting
/// by the key is the walk.
fn bsp_order(s: &mut Stage) {
    s.bind(
        "node_side",
        format!(
            "arrayMap((nx, ny, ndx, ndy) -> {}, k_node_x, k_node_y, k_node_dx, k_node_dy)",
            fixed::point_on_side("v_x", "v_y", "nx", "ny", "ndx", "ndy", 16)
        ),
    );
    s.bind(
        "ssec_key",
        "arrayMap((ns, ss) -> arraySum(arrayMap((n, sd, d) -> \
         if(sd != node_side[n + 1], toUInt64(bitShiftLeft(toUInt64(1), toUInt8(64 - d))), toUInt64(0)), \
         ns, ss, arrayEnumerate(ns))), k_path_nodes, k_path_sides)",
    );
    s.bind(
        "ssec_order",
        "arrayMap(t -> t.2, arraySort(t -> t.1, arrayZip(ssec_key, k_ssec_ids)))",
    );
    // The inverse: where each subsector sits in the walk, counted from one.
    s.bind(
        "ssec_rank",
        "arrayMap(t -> toUInt32(t.2), arraySort(t -> t.1, \
         arrayZip(ssec_order, arrayEnumerate(ssec_order))))",
    );
    // The segs in the order the walk reaches them, and where each
    // subsector's run of segs starts.
    s.bind(
        "seg_seq",
        "arrayFlatten(arrayMap(sc -> arrayMap(k -> toUInt32(k), \
         range(k_ssec_first[sc + 1], k_ssec_first[sc + 1] + k_ssec_num[sc + 1])), ssec_order))",
    );
    s.bind(
        "rank_segbase",
        shifted(
            "arrayCumSum(arrayMap(sc -> k_ssec_num[sc + 1], ssec_order))",
            "toUInt64(0)",
            true,
        ),
    );
}

// ---------------------------------------------------------------------------
// R_AddLine, up to the point where it knows the seg's columns
// ---------------------------------------------------------------------------

/// Projects every seg, in walk order, onto the screen.
///
/// A seg survives when it faces the viewer, when neither edge is wholly off
/// the side of the view, and when it crosses at least one column.
fn seg_project(s: &mut Stage) {
    for (name, coord, view) in [
        ("p_dx1", "k_seg_v1x", "v_x"),
        ("p_dy1", "k_seg_v1y", "v_y"),
        ("p_dx2", "k_seg_v2x", "v_x"),
        ("p_dy2", "k_seg_v2y", "v_y"),
    ] {
        s.bind(
            name,
            format!("arrayMap(sg -> toInt32(toInt64({coord}[sg + 1]) - toInt64({view})), seg_seq)"),
        );
    }
    s.bind(
        "p_ang1",
        format!(
            "arrayMap((dx, dy) -> {}, p_dx1, p_dy1)",
            fixed::point_to_angle("dx", "dy", "k_tantoangle")
        ),
    );
    s.bind(
        "p_ang2",
        format!(
            "arrayMap((dx, dy) -> {}, p_dx2, p_dy2)",
            fixed::point_to_angle("dx", "dy", "k_tantoangle")
        ),
    );
    s.bind(
        "p_span",
        "arrayMap((a, b) -> toUInt32(4294967296 + toUInt64(a) - toUInt64(b)), p_ang1, p_ang2)",
    );
    s.bind(
        "p_a1",
        "arrayMap(a -> toUInt32(4294967296 + toUInt64(a) - toUInt64(v_angle)), p_ang1)",
    );
    s.bind(
        "p_a2",
        "arrayMap(a -> toUInt32(4294967296 + toUInt64(a) - toUInt64(v_angle)), p_ang2)",
    );
    s.bind("k_clip2", "toUInt32(toUInt64(k_clipangle) * 2)");
    s.bind(
        "p_t1",
        "arrayMap(a -> toUInt32(toUInt64(a) + toUInt64(k_clipangle)), p_a1)",
    );
    s.bind(
        "p_t2",
        "arrayMap(a -> toUInt32(4294967296 + toUInt64(k_clipangle) - toUInt64(a)), p_a2)",
    );
    s.bind(
        "p_b1",
        "arrayMap((a, t) -> if(t > k_clip2, k_clipangle, a), p_a1, p_t1)",
    );
    s.bind(
        "p_b2",
        "arrayMap((a, t) -> if(t > k_clip2, toUInt32(4294967296 - toUInt64(k_clipangle)), a), \
         p_a2, p_t2)",
    );
    s.bind(
        "p_x1",
        "arrayMap(a -> k_vatox[bitShiftRight(toUInt32(toUInt64(a) + 1073741824), 19) + 1], p_b1)",
    );
    s.bind(
        "p_x2",
        "arrayMap(a -> k_vatox[bitShiftRight(toUInt32(toUInt64(a) + 1073741824), 19) + 1], p_b2)",
    );
    s.bind(
        "p_vis",
        "arrayMap((sp, t1, t2, x1, x2) -> toUInt8(\
         sp < 2147483648 \
         AND NOT (t1 > k_clip2 AND toUInt32(toUInt64(t1) - toUInt64(k_clip2)) >= sp) \
         AND NOT (t2 > k_clip2 AND toUInt32(toUInt64(t2) - toUInt64(k_clip2)) >= sp) \
         AND x1 != x2), p_span, p_t1, p_t2, p_x1, p_x2)",
    );
}

// ---------------------------------------------------------------------------
// R_AddLine's tail: solid, pass, or nothing at all
// ---------------------------------------------------------------------------

/// The visible segs, with the sector state a wall reads, and the class
/// `R_AddLine` sorts them into: 1 blocks the view, 2 is a window, 0 is a
/// line that draws nothing.
fn seg_classify(s: &mut Stage) {
    for (name, from) in [
        ("q_pos", "arrayEnumerate(seg_seq)"),
        ("q_seg", "seg_seq"),
        ("q_x1", "p_x1"),
        ("q_x2", "p_x2"),
        ("q_ang1", "p_ang1"),
    ] {
        s.bind(
            name,
            format!("arrayFilter((e, vs) -> vs = 1, {from}, p_vis)"),
        );
    }
    s.bind(
        "q_front",
        "arrayMap(sg -> toInt32(k_ssec_sector[k_seg_ssec[sg + 1] + 1]), q_seg)",
    );
    s.bind("q_back", "arrayMap(sg -> k_seg_back[sg + 1], q_seg)");
    s.bind("q_fh", "arrayMap(f -> st_secfloor[f + 1], q_front)");
    s.bind("q_fc", "arrayMap(f -> st_secceil[f + 1], q_front)");
    s.bind("q_ffp", "arrayMap(f -> st_secfloorpic[f + 1], q_front)");
    s.bind("q_fcp", "arrayMap(f -> k_sec_ceilpic[f + 1], q_front)");
    s.bind("q_fll", "arrayMap(f -> st_seclight[f + 1], q_front)");
    s.bind(
        "q_bh",
        "arrayMap(b -> if(b < 0, 0, st_secfloor[b + 1]), q_back)",
    );
    s.bind(
        "q_bc",
        "arrayMap(b -> if(b < 0, 0, st_secceil[b + 1]), q_back)",
    );
    s.bind(
        "q_bfp",
        "arrayMap(b -> if(b < 0, 0, st_secfloorpic[b + 1]), q_back)",
    );
    s.bind(
        "q_bcp",
        "arrayMap(b -> if(b < 0, 0, k_sec_ceilpic[b + 1]), q_back)",
    );
    s.bind(
        "q_bll",
        "arrayMap(b -> if(b < 0, 0, st_seclight[b + 1]), q_back)",
    );
    s.bind(
        "q_sidepack",
        "arrayMap(sg -> st_sidepack[k_seg_side[sg + 1] + 1], q_seg)",
    );
    s.bind(
        "q_midtex_raw",
        "arrayMap(p -> toInt32(toInt16(bitAnd(bitShiftRight(p, 16), 65535))), q_sidepack)",
    );
    s.bind(
        "q_class",
        "arrayMap((bk, fh, fc, bh, bc, ffp, fcp, bfp, bcp, fll, bll, mt) -> toUInt8(multiIf(\
         bk < 0, 1, \
         bc <= fh OR bh >= fc, 1, \
         bc != fc OR bh != fh, 2, \
         bcp = fcp AND bfp = ffp AND bll = fll AND mt = 0, 0, \
         2)), q_back, q_fh, q_fc, q_bh, q_bc, q_ffp, q_fcp, q_bfp, q_bcp, q_fll, q_bll, \
         q_midtex_raw)",
    );
}

// ---------------------------------------------------------------------------
// The solid clip list, as one number per column
// ---------------------------------------------------------------------------

/// When each column first stops letting anything through.
///
/// `solid_time[x]` is the walk position of the earliest solid seg covering
/// column `x`, and 4294967295 for a column nothing closes. A seg at
/// position `p` draws in column `x` exactly when `solid_time[x] >= p`, which
/// is what `R_ClipSolidWallSegment` and `R_ClipPassWallSegment` leave
/// uncovered.
///
/// Segs the walk never reaches are in here too. Each of them sits behind a
/// solid span that closed earlier, so none of them can lower a column's
/// earliest time, and the answer is the same as the recursion's.
fn solid_columns(s: &mut Stage) {
    s.bind(
        "solid_segs",
        "arrayFilter(t -> t.4 = 1, arrayZip(q_pos, q_x1, q_x2, q_class))",
    );
    s.bind(
        "solid_time",
        "arrayMap(x -> arrayMin(arrayConcat([toUInt32(4294967295)], \
         arrayMap(t -> if(x >= t.2 AND x <= t.3 - 1, toUInt32(t.1), toUInt32(4294967295)), \
         solid_segs))), range(320))",
    );
}

// ---------------------------------------------------------------------------
// R_CheckBBox
// ---------------------------------------------------------------------------

/// Which subtrees the walk never enters.
///
/// The recursion tests the far child's bounding box against the clip list
/// before it descends. Here the same test runs for every node at once: the
/// box's columns, the walk position the far subtree starts at, and whether
/// every one of those columns is already closed by then.
fn check_bbox(s: &mut Stage) {
    s.bind("n_side", "node_side");
    s.bind(
        "n_child",
        "arrayMap((ch, sd) -> ch[2 - sd], k_node_child, n_side)",
    );
    s.bind(
        "n_first",
        "arrayMap(ch -> if(bitAnd(ch, 32768) != 0, toUInt32(bitAnd(ch, 32767)), \
         k_range_first[bitAnd(ch, 32767) + 1]), n_child)",
    );
    s.bind(
        "n_last",
        "arrayMap(ch -> if(bitAnd(ch, 32768) != 0, toUInt32(bitAnd(ch, 32767)), \
         k_range_last[bitAnd(ch, 32767) + 1]), n_child)",
    );
    // The far child's four box edges: top, bottom, left, right.
    for (name, k) in [("n_top", 1), ("n_bot", 2), ("n_left", 3), ("n_right", 4)] {
        s.bind(
            name,
            format!("arrayMap((bb, sd) -> bb[(1 - sd) * 4 + {k}], k_node_bbox, n_side)"),
        );
    }
    s.bind(
        "n_boxpos",
        "arrayMap((l, r, t, b) -> \
         (multiIf(v_y >= t, 0, v_y > b, 1, 2) * 4) + multiIf(v_x <= l, 0, v_x < r, 1, 2), \
         n_left, n_right, n_top, n_bot)",
    );
    // `checkcoord` names the two corners the view sees the box by. The
    // order inside a box is top, bottom, left, right, which is what the
    // table's entries index.
    for (name, k) in [("n_cx1", 1), ("n_cy1", 2), ("n_cx2", 3), ("n_cy2", 4)] {
        s.bind(
            name,
            format!(
                "arrayMap((bp, t, b, l, r) -> \
                 [t, b, l, r][k_checkcoord[bp * 4 + {k}] + 1], \
                 n_boxpos, n_top, n_bot, n_left, n_right)"
            ),
        );
    }
    s.bind(
        "n_ang1",
        format!(
            "arrayMap((x, y) -> toUInt32(4294967296 + toUInt64({}) - toUInt64(v_angle)), \
             n_cx1, n_cy1)",
            fixed::point_to_angle(
                "toInt32(toInt64(x) - toInt64(v_x))",
                "toInt32(toInt64(y) - toInt64(v_y))",
                "k_tantoangle"
            )
        ),
    );
    s.bind(
        "n_ang2",
        format!(
            "arrayMap((x, y) -> toUInt32(4294967296 + toUInt64({}) - toUInt64(v_angle)), \
             n_cx2, n_cy2)",
            fixed::point_to_angle(
                "toInt32(toInt64(x) - toInt64(v_x))",
                "toInt32(toInt64(y) - toInt64(v_y))",
                "k_tantoangle"
            )
        ),
    );
    s.bind(
        "n_span",
        "arrayMap((a, b) -> toUInt32(4294967296 + toUInt64(a) - toUInt64(b)), n_ang1, n_ang2)",
    );
    s.bind(
        "n_t1",
        "arrayMap(a -> toUInt32(toUInt64(a) + toUInt64(k_clipangle)), n_ang1)",
    );
    s.bind(
        "n_t2",
        "arrayMap(a -> toUInt32(4294967296 + toUInt64(k_clipangle) - toUInt64(a)), n_ang2)",
    );
    s.bind(
        "n_sx1",
        "arrayMap((a, t) -> k_vatox[bitShiftRight(toUInt32(toUInt64(\
         if(t > k_clip2, k_clipangle, a)) + 1073741824), 19) + 1], n_ang1, n_t1)",
    );
    s.bind(
        "n_sx2",
        "arrayMap((a, t) -> k_vatox[bitShiftRight(toUInt32(toUInt64(\
         if(t > k_clip2, toUInt32(4294967296 - toUInt64(k_clipangle)), a)) + 1073741824), 19) + 1], \
         n_ang2, n_t2)",
    );
    // The walk position the far subtree starts at, counted from one.
    s.bind(
        "n_start",
        "arrayMap((f, l) -> toUInt32(rank_segbase[arrayMin(arraySlice(ssec_rank, \
         toInt32(f) + 1, toInt32(l) - toInt32(f) + 1))] + 1), n_first, n_last)",
    );
    s.bind(
        "n_culled",
        "arrayMap((bp, sp, t1, t2, sx1, sx2, st) -> toUInt8(\
         bp != 5 \
         AND sp < 2147483648 \
         AND ((t1 > k_clip2 AND toUInt32(toUInt64(t1) - toUInt64(k_clip2)) >= sp) \
              OR (t2 > k_clip2 AND toUInt32(toUInt64(t2) - toUInt64(k_clip2)) >= sp) \
              OR sx1 = sx2 \
              OR arrayMax(arraySlice(solid_time, sx1 + 1, sx2 - sx1)) < st)), \
         n_boxpos, n_span, n_t1, n_t2, n_sx1, n_sx2, n_start)",
    );
    s.bind(
        "ssec_culled",
        "arrayMap((ns, ss) -> toUInt8(arrayExists((n, sd) -> \
         sd != node_side[n + 1] AND n_culled[n + 1] = 1, ns, ss)), k_path_nodes, k_path_sides)",
    );
}

// ---------------------------------------------------------------------------
// The fragments the clip list leaves, one drawseg each
// ---------------------------------------------------------------------------

/// Every maximal run of columns a seg still draws in, in the order
/// `R_StoreWallRange` is called for them: seg by seg in walk order, and
/// left to right inside a seg.
fn fragments(s: &mut Stage) {
    // A seg the walk reaches, that draws something.
    s.bind(
        "d_segs",
        "arrayFilter(t -> t.5 != 0 AND ssec_culled[k_seg_ssec[t.2 + 1] + 1] = 0, \
         arrayZip(arrayMap(e -> toUInt32(e), q_pos), q_seg, q_x1, q_x2, q_class, \
         arrayMap(e -> toUInt32(e), arrayEnumerate(q_pos))))",
    );
    // The segs drawing in each column, in walk order.
    s.bind(
        "d_bycol",
        "arrayMap(x -> arrayFilter(t -> t.3 <= x AND t.4 > x AND solid_time[x + 1] >= t.1, \
         d_segs), range(320))",
    );
    s.bind(
        "d_prevcol",
        "arrayPushFront(arrayPopBack(d_bycol), arrayFilter(t -> 0, d_segs))",
    );
    s.bind(
        "d_nextcol",
        "arrayPushBack(arrayPopFront(d_bycol), arrayFilter(t -> 0, d_segs))",
    );
    // A run starts where a seg draws and did not draw in the column before.
    s.bind(
        "d_starts",
        "arrayFlatten(arrayMap((cs, ps, x) -> arrayMap(t -> \
         (t.1 * 512 + toUInt32(x), t.6, toUInt32(x)), \
         arrayFilter(t -> NOT arrayExists(u -> u.1 = t.1, ps), cs)), \
         d_bycol, d_prevcol, range(320)))",
    );
    s.bind(
        "d_ends",
        "arrayFlatten(arrayMap((cs, ns, x) -> arrayMap(t -> (t.1 * 512 + toUInt32(x), toUInt32(x)), \
         arrayFilter(t -> NOT arrayExists(u -> u.1 = t.1, ns), cs)), \
         d_bycol, d_nextcol, range(320)))",
    );
    s.bind("d_start_sorted", "arraySort(t -> t.1, d_starts)");
    s.bind("d_end_sorted", "arraySort(t -> t.1, d_ends)");
    // One drawseg per run: the visible-seg index it came from and the two
    // columns it spans. `MAXDRAWSEGS` is the engine's own ceiling; a run
    // past it clips the view but draws nothing.
    s.bind(
        "ds_all",
        "arrayMap((a, b) -> (a.2, a.3, b.2), d_start_sorted, d_end_sorted)",
    );
    s.bind("ds_list", format!("arraySlice(ds_all, 1, {MAX_DRAWSEGS})"));
    s.bind("ds_qi", "arrayMap(t -> t.1, ds_list)");
    s.bind("ds_x1", "arrayMap(t -> toInt32(t.2), ds_list)");
    s.bind("ds_x2", "arrayMap(t -> toInt32(t.3), ds_list)");
}

// ---------------------------------------------------------------------------
// R_StoreWallRange
// ---------------------------------------------------------------------------

/// Everything a drawseg carries into the wall loop: the distance and scale
/// it is seen at, the three textures and where each one starts, and whether
/// the floor and ceiling behind it are marked.
fn store_wall_range(s: &mut Stage) {
    let gather = |name: &str, src: &str| format!("arrayMap(i -> {src}[i], ds_qi) AS {name}");
    let _ = gather;
    for (name, src) in [
        ("w_seg", "q_seg"),
        ("w_ang1", "q_ang1"),
        ("w_front", "q_front"),
        ("w_back", "q_back"),
        ("w_fh", "q_fh"),
        ("w_fc", "q_fc"),
        ("w_ffp", "q_ffp"),
        ("w_fcp", "q_fcp"),
        ("w_fll", "q_fll"),
        ("w_bh", "q_bh"),
        ("w_bc", "q_bc"),
        ("w_bfp", "q_bfp"),
        ("w_bcp", "q_bcp"),
        ("w_bll", "q_bll"),
        ("w_sidepack", "q_sidepack"),
    ] {
        s.bind(name, format!("arrayMap(i -> {src}[i], ds_qi)"));
    }
    s.bind("w_side", "arrayMap(sg -> k_seg_side[sg + 1], w_seg)");
    s.bind(
        "w_lineflags",
        "arrayMap(sg -> k_line_flags[k_seg_line[sg + 1] + 1], w_seg)",
    );
    s.bind(
        "w_rowoffset",
        "arrayMap(sd -> k_side_rowoffset[sd + 1], w_side)",
    );
    s.bind(
        "w_textureoffset",
        "arrayMap(sd -> st_sideoff[sd + 1], w_side)",
    );
    s.bind("w_segoffset", "arrayMap(sg -> k_seg_offset[sg + 1], w_seg)");
    s.bind(
        "w_toptex_raw",
        "arrayMap(p -> toInt32(toInt16(bitAnd(bitShiftRight(p, 32), 65535))), w_sidepack)",
    );
    s.bind(
        "w_midtex_raw",
        "arrayMap(p -> toInt32(toInt16(bitAnd(bitShiftRight(p, 16), 65535))), w_sidepack)",
    );
    s.bind(
        "w_bottex_raw",
        "arrayMap(p -> toInt32(toInt16(bitAnd(p, 65535))), w_sidepack)",
    );

    // `rw_distance`: the perpendicular distance from the view point to the
    // seg's line.
    s.bind(
        "w_normalangle",
        "arrayMap(sg -> toUInt32(toUInt64(k_seg_angle[sg + 1]) + 1073741824), w_seg)",
    );
    s.bind(
        "w_offsetangle",
        "arrayMap((na, a1) -> least(toUInt32(abs(toInt64(toInt32(\
         toUInt32(4294967296 + toUInt64(na) - toUInt64(a1)))))), toUInt32(1073741824)), \
         w_normalangle, w_ang1)",
    );
    s.bind(
        "w_hyp",
        format!(
            "arrayMap(sg -> {}, w_seg)",
            fixed::point_to_dist(
                "toInt32(toInt64(k_seg_v1x[sg + 1]) - toInt64(v_x))",
                "toInt32(toInt64(k_seg_v1y[sg + 1]) - toInt64(v_y))",
                "k_tantoangle",
                "k_finesine"
            )
        ),
    );
    s.bind(
        "w_dist",
        format!(
            "arrayMap((h, oa) -> {}, w_hyp, w_offsetangle)",
            fixed::fixed_mul(
                "h",
                "k_finesine[bitShiftRight(toUInt32(1073741824 - toUInt64(oa)), 19) + 1]"
            )
        ),
    );

    // The scale at each end of the fragment, and the step between them.
    let scale_at = |col: &str| {
        fixed::scale_from_global_angle(
            &format!("toUInt32(toUInt64(v_angle) + toUInt64(k_xtova[{col} + 1]))"),
            "v_angle",
            "na",
            "rd",
            &CENTER_X_FRAC.to_string(),
            "k_finesine",
        )
    };
    s.bind(
        "w_scale1",
        format!(
            "arrayMap((na, rd, x1) -> {}, w_normalangle, w_dist, ds_x1)",
            scale_at("x1")
        ),
    );
    s.bind(
        "w_scale2",
        format!(
            "arrayMap((na, rd, x1, x2) -> if(x2 > x1, {}, {}), \
             w_normalangle, w_dist, ds_x1, ds_x2)",
            scale_at("x2"),
            scale_at("x1")
        ),
    );
    // A one-column fragment leaves `rw_scalestep` at whatever the last
    // fragment set. Nothing reads it: the loop runs once and every stepped
    // value is used before its step is added.
    s.bind(
        "w_scalestep",
        "arrayMap((s1, s2, x1, x2) -> if(x2 > x1, toInt32(intDiv(toInt64(s2) - toInt64(s1), \
         toInt64(x2 - x1))), toInt32(0)), w_scale1, w_scale2, ds_x1, ds_x2)",
    );

    // Texture boundaries, and the marks. `worldtop` moves to the back
    // sector's ceiling when both sides are sky, which is the hack that lets
    // an outdoor ceiling change height without a seam.
    s.bind(
        "w_worldbottom",
        "arrayMap(fh -> toInt32(toInt64(fh) - toInt64(v_z)), w_fh)",
    );
    s.bind(
        "w_worldtop",
        "arrayMap((fc, bc, fcp, bcp, bk) -> if(bk >= 0 AND fcp = k_skyflat AND bcp = k_skyflat, \
         toInt32(toInt64(bc) - toInt64(v_z)), toInt32(toInt64(fc) - toInt64(v_z))), \
         w_fc, w_bc, w_fcp, w_bcp, w_back)",
    );
    s.bind(
        "w_worldhigh",
        "arrayMap((bc, bk) -> if(bk < 0, toInt32(0), toInt32(toInt64(bc) - toInt64(v_z))), \
         w_bc, w_back)",
    );
    s.bind(
        "w_worldlow",
        "arrayMap((bh, bk) -> if(bk < 0, toInt32(0), toInt32(toInt64(bh) - toInt64(v_z))), \
         w_bh, w_back)",
    );
    s.bind(
        "w_markfloor0",
        "arrayMap((bk, wl, wb, bfp, ffp, bll, fll, fh, fc, bh, bc) -> toUInt8(multiIf(\
         bk < 0, 1, \
         bc <= fh OR bh >= fc, 1, \
         wl != wb OR bfp != ffp OR bll != fll, 1, 0)), \
         w_back, w_worldlow, w_worldbottom, w_bfp, w_ffp, w_bll, w_fll, w_fh, w_fc, w_bh, w_bc)",
    );
    s.bind(
        "w_markceiling0",
        "arrayMap((bk, wh, wt, bcp, fcp, bll, fll, fh, fc, bh, bc) -> toUInt8(multiIf(\
         bk < 0, 1, \
         bc <= fh OR bh >= fc, 1, \
         wh != wt OR bcp != fcp OR bll != fll, 1, 0)), \
         w_back, w_worldhigh, w_worldtop, w_bcp, w_fcp, w_bll, w_fll, w_fh, w_fc, w_bh, w_bc)",
    );
    // A plane on the wrong side of the view plane is never seen.
    s.bind(
        "w_markfloor",
        "arrayMap((m, fh) -> toUInt8(m = 1 AND fh < v_z), w_markfloor0, w_fh)",
    );
    s.bind(
        "w_markceiling",
        "arrayMap((m, fc, fcp) -> toUInt8(m = 1 AND (fc > v_z OR fcp = k_skyflat)), \
         w_markceiling0, w_fc, w_fcp)",
    );

    s.bind(
        "w_midtex",
        "arrayMap((bk, mt) -> if(bk < 0, st_texxlat[mt + 1], toInt32(0)), w_back, w_midtex_raw)",
    );
    s.bind(
        "w_toptex",
        "arrayMap((bk, wh, wt, tt) -> if(bk >= 0 AND wh < wt, st_texxlat[tt + 1], toInt32(0)), \
         w_back, w_worldhigh, w_worldtop, w_toptex_raw)",
    );
    s.bind(
        "w_bottex",
        "arrayMap((bk, wl, wb, bt) -> if(bk >= 0 AND wl > wb, st_texxlat[bt + 1], toInt32(0)), \
         w_back, w_worldlow, w_worldbottom, w_bottex_raw)",
    );
    s.bind(
        "w_masked",
        "arrayMap((bk, mt) -> toUInt8(bk >= 0 AND mt != 0), w_back, w_midtex_raw)",
    );
    s.bind(
        "w_midmid",
        "arrayMap((fl, fh, mt, wt, ro) -> toInt32(toInt64(ro) + toInt64(\
         if(bitAnd(fl, 16) != 0, toInt32(toInt64(fh) + toInt64(k_tex_height[mt + 1]) \
         - toInt64(v_z)), wt))), w_lineflags, w_fh, w_midtex_raw, w_worldtop, w_rowoffset)",
    );
    s.bind(
        "w_topmid",
        "arrayMap((fl, bc, tt, wt, ro) -> toInt32(toInt64(ro) + toInt64(\
         if(bitAnd(fl, 8) != 0, wt, toInt32(toInt64(bc) + toInt64(k_tex_height[tt + 1]) \
         - toInt64(v_z))))), w_lineflags, w_bc, w_toptex_raw, w_worldtop, w_rowoffset)",
    );
    s.bind(
        "w_botmid",
        "arrayMap((fl, wt, wl, ro) -> toInt32(toInt64(ro) + toInt64(\
         if(bitAnd(fl, 16) != 0, wt, wl))), w_lineflags, w_worldtop, w_worldlow, w_rowoffset)",
    );

    // `rw_offset`: where in the texture the fragment's left edge sits.
    s.bind(
        "w_offsetangle2",
        "arrayMap((na, a1) -> toUInt32(4294967296 + toUInt64(na) - toUInt64(a1)), \
         w_normalangle, w_ang1)",
    );
    s.bind(
        "w_offsetangle3",
        "arrayMap(oa -> least(if(oa > 2147483648, toUInt32(4294967296 - toUInt64(oa)), oa), \
         toUInt32(1073741824)), w_offsetangle2)",
    );
    s.bind(
        "w_offset",
        format!(
            "arrayMap((h, oa3, oa2, to, so) -> toInt32(toInt64(to) + toInt64(so) + toInt64(\
             if(oa2 < 2147483648, toInt32(-toInt64({m})), {m}))), \
             w_hyp, w_offsetangle3, w_offsetangle2, w_textureoffset, w_segoffset)",
            m = fixed::fixed_mul("h", "k_finesine[bitShiftRight(oa3, 19) + 1]")
        ),
    );
    s.bind(
        "w_centerangle",
        "arrayMap(na -> toUInt32(4294967296 + 1073741824 + toUInt64(v_angle) - toUInt64(na)), \
         w_normalangle)",
    );
    // The light row a wall reads, with the engine's tilt for a wall that
    // runs straight along one axis. 255 means the player carries a fixed
    // colormap and the row is not consulted.
    s.bind(
        "w_light",
        "arrayMap((ll, sg) -> if(v_fixedcolormap != 0, toUInt8(255), toUInt8(least(greatest(\
         toInt64(bitShiftRight(ll, 4)) + toInt64(v_extralight) \
         + multiIf(k_seg_v1y[sg + 1] = k_seg_v2y[sg + 1], -1, \
                   k_seg_v1x[sg + 1] = k_seg_v2x[sg + 1], 1, 0), 0), 15))), w_fll, w_seg)",
    );

    // The stepped edges, in the wall loop's own twelve-bit scale.
    for (name, world, frac) in [
        ("w_topstep", "w_worldtop", false),
        ("w_topfrac", "w_worldtop", true),
        ("w_botstep", "w_worldbottom", false),
        ("w_botfrac", "w_worldbottom", true),
        ("w_pixhighstep", "w_worldhigh", false),
        ("w_pixhigh", "w_worldhigh", true),
        ("w_pixlowstep", "w_worldlow", false),
        ("w_pixlow", "w_worldlow", true),
    ] {
        let expr = if frac {
            format!(
                "arrayMap((wd, sc) -> toInt32({CENTER_Y_FRAC_4} - toInt64({})), {world}, w_scale1)",
                fixed::fixed_mul("bitShiftRight(wd, 4)", "sc")
            )
        } else {
            format!(
                "arrayMap((wd, ss) -> toInt32(-toInt64({})), {world}, w_scalestep)",
                fixed::fixed_mul("ss", "bitShiftRight(wd, 4)")
            )
        };
        s.bind(name, expr);
    }

    // What the drawseg leaves of a sprite behind it. A single-sided wall hides
    // both ends of one; a two-sided wall hides an end only where its own floor
    // or ceiling cuts across. `w_topconst` and `w_botconst` mark the two cases
    // where the clip is the edge of the view rather than the wall loop's own.
    s.bind(
        "w_topconst",
        "arrayMap((bk, bh, fc) -> toUInt8(bk < 0 OR bh >= fc), w_back, w_bh, w_fc)",
    );
    s.bind(
        "w_botconst",
        "arrayMap((bk, bc, fh) -> toUInt8(bk < 0 OR bc <= fh), w_back, w_bc, w_fh)",
    );
    s.bind(
        "w_sil",
        "arrayMap((bk, fh, fc, bh, bc, mk) -> toUInt8(\
         if(bk < 0 OR fh > bh OR bh > v_z OR bc <= fh OR mk = 1, 1, 0) \
         + if(bk < 0 OR fc < bc OR bc < v_z OR bh >= fc OR mk = 1, 2, 0)), \
         w_back, w_fh, w_fc, w_bh, w_bc, w_masked)",
    );
    s.bind(
        "w_bsil",
        "arrayMap((bk, fh, bh, bc) -> multiIf(bk < 0, toInt32(2147483647), \
         bc <= fh, toInt32(2147483647), fh > bh, fh, toInt32(2147483647)), \
         w_back, w_fh, w_bh, w_bc)",
    );
    s.bind(
        "w_tsil",
        "arrayMap((bk, fc, bh, bc) -> multiIf(bk < 0, toInt32(-2147483648), \
         bh >= fc, toInt32(-2147483648), fc < bc, fc, toInt32(-2147483648)), \
         w_back, w_fc, w_bh, w_bc)",
    );
}

// ---------------------------------------------------------------------------
// R_RenderSegLoop
// ---------------------------------------------------------------------------

/// The per-column clip walk.
///
/// `ceilingclip` and `floorclip` start at -1 and 168 and only ever close in,
/// but a bottom wall clamps against the ceiling the top wall just wrote, so
/// a column has to be walked drawseg by drawseg. The fold does that and
/// records, for each drawseg the column meets, the two clips as they stood
/// before it and the rows the drawseg then covers.
fn render_seg_loop(s: &mut Stage) {
    // What a drawseg hands the fold.
    let fields: &[(&str, &str)] = &[
        ("ds", "toUInt32(i)"),
        ("x1", "ds_x1[i]"),
        ("x2", "ds_x2[i]"),
        ("topfrac", "w_topfrac[i]"),
        ("topstep", "w_topstep[i]"),
        ("botfrac", "w_botfrac[i]"),
        ("botstep", "w_botstep[i]"),
        ("pixhigh", "w_pixhigh[i]"),
        ("pixhighstep", "w_pixhighstep[i]"),
        ("pixlow", "w_pixlow[i]"),
        ("pixlowstep", "w_pixlowstep[i]"),
        ("midtex", "w_midtex[i]"),
        ("toptex", "w_toptex[i]"),
        ("bottex", "w_bottex[i]"),
        ("mkc", "toUInt8(w_markceiling[i])"),
        ("mkf", "toUInt8(w_markfloor[i])"),
        ("midmid", "w_midmid[i]"),
        ("topmid", "w_topmid[i]"),
        ("botmid", "w_botmid[i]"),
        ("scale1", "w_scale1[i]"),
        ("scalestep", "w_scalestep[i]"),
        ("rwoffset", "w_offset[i]"),
        ("rwdist", "w_dist[i]"),
        ("centerangle", "w_centerangle[i]"),
        ("light", "w_light[i]"),
    ];
    let idx = |name: &str| {
        1 + fields
            .iter()
            .position(|(f, _)| *f == name)
            .expect("a drawseg field the fold reads")
    };
    s.bind(
        "cw_in",
        format!(
            "arrayMap(i -> ({}), arrayEnumerate(ds_qi))",
            fields
                .iter()
                .map(|(_, e)| *e)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );

    // The stepped values at this column, and the row bounds they give.
    let at = |field: &str, step: &str| {
        format!(
            "toInt32(toInt64(t.{}) + toInt64(t.{}) * toInt64(x - t.{}))",
            idx(field),
            idx(step),
            idx("x1")
        )
    };
    let topfrac = at("topfrac", "topstep");
    let botfrac = at("botfrac", "botstep");
    let pixhigh = at("pixhigh", "pixhighstep");
    let pixlow = at("pixlow", "pixlowstep");
    let scale = format!(
        "toInt32(toInt64(t.{}) + toInt64(t.{}) * toInt64(x - t.{}))",
        idx("scale1"),
        idx("scalestep"),
        idx("x1")
    );
    // `yl` cannot rise above the ceiling already drawn, `yh` cannot fall
    // below the floor.
    let yl = format!("greatest(bitShiftRight(toInt32(toInt64({topfrac}) + 4095), 12), acc.1 + 1)");
    let yh = format!("least(bitShiftRight({botfrac}, 12), acc.2 - 1)");
    // A top wall ends where the back ceiling is, a bottom wall starts where
    // the back floor is, each clamped into what is left of the column.
    let midlo = format!("least(bitShiftRight({pixhigh}, 12), acc.2 - 1)");
    let botlo = format!("greatest(bitShiftRight(toInt32(toInt64({pixlow}) + 4095), 12), 0)");

    // The new clips. A single-sided wall closes the column for good.
    let new_ceil = format!(
        "multiIf(t.{mid} != 0, {vh}, \
         t.{top} != 0, if({midlo} >= {yl}, {midlo}, {yl} - 1), \
         t.{mkc} = 1, {yl} - 1, acc.1)",
        mid = idx("midtex"),
        top = idx("toptex"),
        mkc = idx("mkc"),
        vh = VIEW_HEIGHT,
    );
    // The bottom wall clamps against the ceiling the top wall just set.
    let botlo_clamped = format!("greatest({botlo}, {new_ceil} + 1)");
    let new_floor = format!(
        "multiIf(t.{mid} != 0, -1, \
         t.{bot} != 0, if({botlo_clamped} <= {yh}, {botlo_clamped}, {yh} + 1), \
         t.{mkf} = 1, {yh} + 1, acc.2)",
        mid = idx("midtex"),
        bot = idx("bottex"),
        mkf = idx("mkf"),
    );
    // The texture column the wall reads, and the scale index the colormap
    // comes from.
    let texcol = format!(
        "bitShiftRight(toInt32(toInt64(t.{off}) - toInt64({m})), 16)",
        off = idx("rwoffset"),
        m = fixed::fixed_mul(
            &format!(
                "k_finetangent[bitShiftRight(toUInt32(toUInt64(t.{}) + toUInt64(k_xtova[x + 1])), 19) + 1]",
                idx("centerangle")
            ),
            &format!("t.{}", idx("rwdist"))
        )
    );

    // Every field carries its own cast: `arrayFold` needs the lambda's
    // result to match the accumulator exactly, and ClickHouse widens as it
    // goes.
    let out = [
        "toUInt16(x)".to_owned(),
        format!("toUInt32(t.{})", idx("ds")),
        format!("toInt32({yl})"),
        format!("toInt32({yh})"),
        format!("toInt32({midlo})"),
        format!("toInt32({botlo_clamped})"),
        "toInt32(acc.1)".to_owned(),
        "toInt32(acc.2)".to_owned(),
        format!("toInt32({texcol})"),
        format!("toInt32({scale})"),
        format!("toInt32(t.{})", idx("midtex")),
        format!("toInt32(t.{})", idx("toptex")),
        format!("toInt32(t.{})", idx("bottex")),
        format!("toInt32(t.{})", idx("midmid")),
        format!("toInt32(t.{})", idx("topmid")),
        format!("toInt32(t.{})", idx("botmid")),
        format!("toUInt8(t.{})", idx("light")),
        format!("toUInt8(t.{})", idx("mkc")),
        format!("toUInt8(t.{})", idx("mkf")),
    ]
    .join(", ");

    let out_type = "Array(Tuple(UInt16, UInt32, Int32, Int32, Int32, Int32, Int32, Int32, \
                    Int32, Int32, Int32, Int32, Int32, Int32, Int32, Int32, UInt8, UInt8, UInt8))";

    s.bind(
        "cw",
        format!(
            "arrayFlatten(arrayMap(x -> arrayFold((acc, t) -> \
             (toInt32({new_ceil}), toInt32({new_floor}), arrayPushBack(acc.3, ({out}))), \
             arrayFilter(t -> t.{x1} <= x AND t.{x2} >= x, cw_in), \
             (toInt32(-1), toInt32({VIEW_HEIGHT}), CAST([], '{out_type}'))).3, range(320)))",
            x1 = idx("x1"),
            x2 = idx("x2"),
        ),
    );
}

/// The names of the fold's output fields, in order.
const CW: &[&str] = &[
    "x", "ds", "yl", "yh", "midlo", "botlo", "cc0", "fc0", "texcol", "scale", "midtex", "toptex",
    "bottex", "midmid", "topmid", "botmid", "light", "mkc", "mkf",
];

/// `t.N` for one of the wall loop's output fields.
fn cw(field: &str) -> String {
    let n = 1 + CW
        .iter()
        .position(|f| *f == field)
        .expect("a wall loop field");
    format!("t.{n}")
}

// ---------------------------------------------------------------------------
// R_FindPlane and R_CheckPlane
// ---------------------------------------------------------------------------

/// The visplanes, and which one each marked column belongs to.
///
/// A subsector's floor and ceiling name a plane by height, picture and light
/// level, and `R_FindPlane` hands back the first plane already carrying that
/// name. `R_CheckPlane` splits it when the columns the drawseg is about to
/// mark overlap columns the plane has marked already, so one name can end up
/// as several planes. The split matters: a span's texture is stepped from
/// the column it starts at, so where a run of columns breaks changes pixels.
fn visplanes(s: &mut Stage) {
    // A subsector names its floor and its ceiling by height, picture and
    // light level. The sky is one plane whatever its height, which is the
    // remap `R_FindPlane` makes before it looks.
    s.bind("pl_ssec", "arrayMap(sg -> k_seg_ssec[sg + 1], w_seg)");
    s.bind(
        "pl_ckey",
        "arrayMap((fc, fcp, fll) -> if(fcp = k_skyflat, \
         (toInt32(0), toInt32(k_skyflat), toInt32(0)), \
         (fc, toInt32(fcp), toInt32(fll))), w_fc, w_fcp, w_fll)",
    );
    s.bind(
        "pl_fkey",
        "arrayMap((fh, ffp, fll) -> (fh, toInt32(ffp), toInt32(fll)), w_fh, w_ffp, w_fll)",
    );

    // The rows the wall loop leaves for each plane, one range per drawseg
    // and column.
    let ceil_top = format!("{} + 1", cw("cc0"));
    let ceil_bottom = format!("least({} - 1, {} - 1)", cw("yl"), cw("fc0"));
    s.bind(
        "mk_ceil",
        format!(
            "arrayFilter(m -> m.3 <= m.4, arrayMap(t -> ({ds}, toInt32({x}), {ceil_top}, \
             {ceil_bottom}), arrayFilter(t -> {mkc} = 1, cw)))",
            ds = cw("ds"),
            x = cw("x"),
            mkc = cw("mkc"),
        ),
    );
    let floor_top = format!("greatest({} + 1, {} + 1)", cw("yh"), cw("cc0"));
    let floor_bottom = format!("{} - 1", cw("fc0"));
    s.bind(
        "mk_floor",
        format!(
            "arrayFilter(m -> m.3 <= m.4, arrayMap(t -> ({ds}, toInt32({x}), {floor_top}, \
             {floor_bottom}), arrayFilter(t -> {mkf} = 1, cw)))",
            ds = cw("ds"),
            x = cw("x"),
            mkf = cw("mkf"),
        ),
    );

    // One event per drawseg that marked anything: the plane it names, the
    // subsector it belongs to, the columns it spans and the columns it
    // marked. The event number orders ceilings before floors inside a
    // drawseg, which is the order the wall loop writes them.
    for (name, marks, key, kind) in [
        ("pl_ev_ceil", "mk_ceil", "pl_ckey", 0),
        ("pl_ev_floor", "mk_floor", "pl_fkey", 1),
    ] {
        s.bind(
            name,
            format!(
                "arrayFilter(e -> length(e.6) > 0, arrayMap(d -> (\
                 {key}[d], pl_ssec[d], toUInt32(d) * 2 + {kind}, ds_x1[d], ds_x2[d], \
                 arrayMap(m -> m.2, arrayFilter(m -> m.1 = toUInt32(d), {marks}))), \
                 arrayEnumerate(ds_qi)))"
            ),
        );
    }
    s.bind("pl_ev", "arrayConcat(pl_ev_ceil, pl_ev_floor)");
    // A floor plane and a ceiling plane never carry the same name: one
    // needs its height below the view point and the other above it.
    s.bind("pl_keys", "arrayDistinct(arrayMap(e -> e.1, pl_ev))");

    // `R_CheckPlane`, per plane name, over that name's events in the order
    // the wall loop reached them. A new subsector starts again at the name's
    // first plane; a drawseg whose columns overlap what the current plane
    // has already marked starts a new one.
    //
    // The state is the first plane's marked columns, the current plane's,
    // whether the current plane is the first, its number, the subsector the
    // last event came from, the next free number, and the answer so far.
    let body = "(if(fresh = 1 AND NOT split, arrayConcat(base, e.6), acc.1), \
                if(split, e.6, arrayConcat(base, e.6)), \
                toUInt8(if(split, 0, fresh)), \
                toUInt32(num), toUInt32(e.2), toUInt32(if(split, acc.6 + 1, acc.6)), \
                arrayPushBack(acc.7, (toUInt32(e.3), toUInt32(num))))";
    let step = let_in(
        &[
            ("base", "if(e.2 != acc.5, acc.1, acc.2)".to_owned()),
            ("fresh", "if(e.2 != acc.5, toUInt8(1), acc.3)".to_owned()),
        ],
        &let_in(
            &[(
                "split",
                "arrayExists(c -> c >= e.4 AND c <= e.5, base)".to_owned(),
            )],
            &let_in(
                &[(
                    "num",
                    "if(split, acc.6, if(fresh = 1, toUInt32(0), acc.4))".to_owned(),
                )],
                body,
            ),
        ),
    );
    s.bind(
        "pl_split",
        format!(
            "arrayMap(k -> arrayFold((acc, e) -> {step}, \
             arraySort(e -> e.3, arrayFilter(e -> e.1 = k, pl_ev)), \
             (CAST([], 'Array(Int32)'), CAST([], 'Array(Int32)'), toUInt8(1), toUInt32(0), \
              toUInt32(4294967295), toUInt32(1), \
              CAST([], 'Array(Tuple(UInt32, UInt32))'))).7, pl_keys)"
        ),
    );

    // Every plane the frame drew, numbered from zero, with the name behind
    // it and the event that reached it.
    s.bind(
        "pl_ev_inst",
        "arrayFlatten(arrayMap((res, ki) -> arrayMap(o -> \
         (o.1, toUInt32(ki - 1) * 1024 + o.2), res), pl_split, arrayEnumerate(pl_split)))",
    );
    s.bind(
        "pl_inst_num",
        "arrayDistinct(arrayMap(o -> o.2, pl_ev_inst))",
    );
    s.bind(
        "pl_inst_name",
        "arrayMap(n -> pl_keys[intDiv(n, 1024) + 1], pl_inst_num)",
    );
    // The plane each event landed on, by event number, counted from one so
    // that zero means an event that marked nothing.
    s.bind(
        "pl_of_ev",
        "arrayMap(e -> arrayMax(arrayMap(o -> \
         if(o.1 = toUInt32(e), toUInt32(indexOf(pl_inst_num, o.2)), toUInt32(0)), pl_ev_inst)), \
         range(2 * length(ds_qi) + 2))",
    );

    // The marked rows again, now behind the plane that owns them. `at`
    // packs the plane and the column so one sort brings a plane's columns
    // together in column order.
    for (name, marks, kind) in [("pm_ceil", "mk_ceil", 0), ("pm_floor", "mk_floor", 1)] {
        s.bind(
            name,
            format!(
                "arrayMap(m -> ((pl_of_ev[m.1 * 2 + {kind} + 1] - 1) * 512 + toUInt32(m.2), \
                 m.3, m.4), {marks})"
            ),
        );
    }
    s.bind("pm", "arraySort(t -> t.1, arrayConcat(pm_ceil, pm_floor))");
    s.bind(
        "pm_prev",
        shifted(
            "pm",
            "(toUInt32(4294967295), toInt32(255), toInt32(255))",
            true,
        ),
    );
    s.bind(
        "pm_next",
        shifted(
            "pm",
            "(toUInt32(4294967295), toInt32(255), toInt32(255))",
            false,
        ),
    );
    // A span starts in a column where a row is covered and the column to its
    // left is not, and ends where the column to its right is not. What the
    // neighbour's range leaves of this one is at most two runs of rows, and
    // a neighbour that is not the adjacent column covers nothing.
    for (name, side, adjacent) in [
        ("sp_starts", "pm_prev", "o.1 + 1 = c.1"),
        ("sp_ends", "pm_next", "c.1 + 1 = o.1"),
    ] {
        let top = format!("if({adjacent}, o.2, 255)");
        let bottom = format!("if({adjacent}, o.3, 255)");
        s.bind(
            name,
            format!(
                "arrayFlatten(arrayMap((c, o) -> arrayMap(y -> \
                 (toUInt64(intDiv(c.1, 512)) * 100000 + toUInt64(y) * 320 + toUInt64(c.1 % 512), \
                  toInt32(c.1 % 512)), \
                 arrayConcat(\
                   range(c.2, greatest(least(c.3, {top} - 1) + 1, c.2)), \
                   range(greatest(c.2, {bottom} + 1), \
                         greatest(c.3 + 1, greatest(c.2, {bottom} + 1))))), \
                 pm, {side}))"
            ),
        );
    }
    s.bind("sp_start_sorted", "arraySort(t -> t.1, sp_starts)");
    s.bind("sp_end_sorted", "arraySort(t -> t.1, sp_ends)");
    // One span per pair: the plane, the row, and the two columns.
    s.bind(
        "spans",
        "arrayMap((a, b) -> (toUInt32(intDiv(a.1, 100000)), \
         toInt32(intDiv(a.1 % 100000, 320)), a.2, b.2), sp_start_sorted, sp_end_sorted)",
    );
}

// ---------------------------------------------------------------------------
// The pixels
// ---------------------------------------------------------------------------

/// `R_DrawColumn` for each wall a column draws.
fn wall_pixels(s: &mut Stage) {
    let run_type = "Array(Tuple(Int32, Int32, Int32, Int32, Int32, UInt8, Int32))";
    let column_run = |lo: String, hi: String, tex: String, mid: String| {
        format!(
            "if({tex} != 0 AND {lo} <= {hi}, \
             [(toInt32({lo}), toInt32({hi}), {tex}, {mid}, {sc}, {li}, {tc})], \
             CAST([], '{run_type}'))",
            sc = cw("scale"),
            li = cw("light"),
            tc = cw("texcol"),
        )
    };
    s.bind(
        "wall_runs",
        format!(
            "arrayFlatten(arrayMap(t -> arrayMap(r -> (toInt32({x}), r), \
             arrayConcat({single}, {top}, {bot})), cw))",
            x = cw("x"),
            single = column_run(cw("yl"), cw("yh"), cw("midtex"), cw("midmid")),
            top = column_run(
                cw("yl"),
                format!("least({}, {})", cw("midlo"), cw("yh")),
                cw("toptex"),
                cw("topmid")
            ),
            bot = column_run(cw("botlo"), cw("yh"), cw("bottex"), cw("botmid")),
        ),
    );
    // `dc_iscale` is an unsigned divide, and the texel index is seven bits
    // of the frac's whole part, which wraps in thirty-two.
    let level = "if(t.2.6 = 255, toUInt32(v_fixedcolormap), \
                 toUInt32(k_scalelight[toUInt32(t.2.6) * 48 \
                 + toUInt32(least(bitShiftRight(t.2.5, 12), 47)) + 1]))";
    let base = "(toUInt32(k_tex_base[t.2.3 + 1]) \
                + toUInt32(bitAnd(t.2.7, toInt32(k_tex_mask[t.2.3 + 1]))))";
    let iscale = "intDiv(toUInt32(4294967295), toUInt32(t.2.5))";
    let row = format!(
        "toUInt32(bitAnd(bitShiftRight(toUInt32(toInt64(t.2.4) \
         + toInt64(y - {CENTER_Y}) * toInt64({iscale})), 16), 127))"
    );
    let texel = pool("k_texpool", &format!("{base} * 128 + {row}"));
    let cmap = pool("k_colormap", &format!("{level} * 256 + {texel}"));
    s.bind(
        "wall_px",
        format!(
            "arrayFlatten(arrayMap(t -> arrayMap(y -> (\
             toUInt64(y * {VIEW_WIDTH} + t.1) * 1048576, \
             {cmap}), \
             range(t.2.1, t.2.2 + 1)), wall_runs))"
        ),
    );
}

/// One byte out of a pixel pool. A pool is a string, so a lookup is a
/// one-byte substring rather than an array element.
fn pool(name: &str, index: &str) -> String {
    format!("reinterpretAsUInt8(substring({name}, {index} + 1, 1))")
}

/// `R_MapPlane` and `R_DrawSpan`, over every span of every plane but the
/// sky.
///
/// A span walks one packed number: the x half in the top sixteen bits, the y
/// half in the bottom, and the y half's carry runs into the x half. That
/// carry is what the engine's own loop does, so the addition wraps in
/// thirty-two bits and the texel comes out of the two halves by shift and
/// mask.
fn plane_pixels(s: &mut Stage) {
    s.bind(
        "fp_spans",
        "arrayFilter(t -> pl_inst_name[t.1 + 1].2 != k_skyflat, spans)",
    );
    // What `R_MapPlane` computes once, at the span's first column.
    let distance = fixed::fixed_mul(
        "toInt32(abs(toInt64(pl_inst_name[t.1 + 1].1) - toInt64(v_z)))",
        "k_yslope[t.2 + 1]",
    );
    let length = fixed::fixed_mul("d", "k_distscale[t.3 + 1]");
    let fine = "bitShiftRight(toUInt32(toUInt64(v_angle) + toUInt64(k_xtova[t.3 + 1])), 19)";
    let xfrac = format!(
        "toInt32(toInt64(v_x) + toInt64({}))",
        fixed::fixed_mul(&format!("k_finesine[{fine} + 2049]"), &length)
    );
    let yfrac = format!(
        "toInt32(-toInt64(v_y) - toInt64({}))",
        fixed::fixed_mul(&format!("k_finesine[{fine} + 1]"), &length)
    );
    let pack = |x: &str, y: &str| {
        format!(
            "bitOr(bitAnd(toUInt32(bitShiftLeft(toInt64({x}), 10)), toUInt32(4294901760)), \
             toUInt32(bitAnd(bitShiftRight({y}, 6), 65535)))"
        )
    };
    let body = format!(
        "(t, {level}, {position}, {step}, {flat})",
        level = "if(v_fixedcolormap != 0, toUInt32(v_fixedcolormap), \
                 toUInt32(k_zlight[toUInt32(least(greatest(\
                 toInt64(bitShiftRight(pl_inst_name[t.1 + 1].3, 4)) \
                 + toInt64(v_extralight), 0), 15)) * 128 \
                 + toUInt32(least(bitShiftRight(d, 20), 127)) + 1]))",
        position = pack(&xfrac, &yfrac),
        step = pack(
            &fixed::fixed_mul("d", "v_basex"),
            &fixed::fixed_mul("d", "v_basey")
        ),
        flat = "toUInt32(st_flatxlat[pl_inst_name[t.1 + 1].2 + 1])",
    );
    s.bind(
        "fp",
        format!(
            "arrayMap(t -> {}, fp_spans)",
            let_in(&[("d", distance)], &body)
        ),
    );
    let position = "toUInt32(toUInt64(f.3) + toUInt64(f.4) * toUInt64(x - f.1.3))";
    let texel = pool(
        "k_flatpool",
        &format!(
            "f.5 * 4096 + toUInt32(bitOr(bitShiftRight({position}, 26), \
             bitAnd(bitShiftRight({position}, 4), 4032)))"
        ),
    );
    let cmap = pool("k_colormap", &format!("f.2 * 256 + {texel}"));
    s.bind(
        "flat_px",
        format!(
            "arrayFlatten(arrayMap(f -> arrayMap(x -> (\
             toUInt64(f.1.2 * {VIEW_WIDTH} + x) * 1048576, \
             {cmap}), \
             range(f.1.3, f.1.4 + 1)), fp))"
        ),
    );
}

/// The sky, which is a wall column at a fixed scale and full brightness.
fn sky_pixels(s: &mut Stage) {
    s.bind(
        "sky_marks",
        "arrayFilter(m -> pl_inst_name[intDiv(m.1, 512) + 1].2 = k_skyflat, pm)",
    );
    let base = "(toUInt32(k_tex_base[k_skytex + 1]) + toUInt32(bitAnd(toInt32(\
                bitShiftRight(toUInt32(toUInt64(v_angle) \
                + toUInt64(k_xtova[toUInt32(m.1 % 512) + 1])), 22)), \
                toInt32(k_tex_mask[k_skytex + 1]))))";
    let row = format!(
        "toUInt32(bitAnd(bitShiftRight(toUInt32(toInt64(k_skymid) \
         + toInt64(y - {CENTER_Y}) * 65536), 16), 127))"
    );
    let texel = pool("k_texpool", &format!("{base} * 128 + {row}"));
    // The sky is always drawn through the first colormap.
    let cmap = pool("k_colormap", &texel);
    s.bind(
        "sky_px",
        format!(
            "arrayFlatten(arrayMap(m -> arrayMap(y -> (\
             toUInt64(y * {VIEW_WIDTH} + toInt32(m.1 % 512)) * 1048576, \
             {cmap}), \
             range(m.2, m.3 + 1)), sky_marks))"
        ),
    );
}

// ---------------------------------------------------------------------------
// R_AddSprites, R_ProjectSprite and R_SortVisSprites
// ---------------------------------------------------------------------------

/// Every thing the frame draws, in the order `R_DrawMasked` draws them.
///
/// `R_AddSprites` runs once per sector, at the first subsector of it the walk
/// reaches, and walks that sector's own list of things. The list is newest
/// first, because `P_SetThingPosition` pushes onto its head. What comes out is
/// sorted by scale, smallest first, so the far ones are drawn first.
fn sprites(s: &mut Stage) {
    s.state("st_mz", "m_z");
    s.state("st_mflags", "m_flags");
    s.state("st_msprite", "m_sprite");
    s.state("st_mframe", "m_frame");
    s.state("st_msubsector", "m_subsector");
    s.state("st_mlinkseq", "m_linkseq");

    // The sectors the walk reached, in the order it first reached one.
    s.bind(
        "vis_culled",
        "arrayMap(sc -> ssec_culled[sc + 1], ssec_order)",
    );
    s.bind(
        "vis_ssec",
        "arrayFilter((sc, c) -> c = 0, ssec_order, vis_culled)",
    );
    s.bind(
        "sec_order",
        "arrayDistinct(arrayMap(sc -> k_ssec_sector[sc + 1], vis_ssec))",
    );
    // Where each sector sits in that order, counted from one. Zero is a
    // sector the walk never reached, whose things are never projected.
    s.bind(
        "sec_rank",
        "arrayMap(i -> toUInt32(indexOf(sec_order, toUInt32(i - 1))), arrayEnumerate(st_secfloor))",
    );
    s.bind(
        "th_all",
        "arrayMap(i -> (toUInt32(i), \
         toUInt32(if(st_msubsector[i] < 0, 0, sec_rank[k_ssec_sector[st_msubsector[i] + 1] + 1])), \
         st_mlinkseq[i]), arrayEnumerate(st_mx))",
    );
    // A thing off the sector lists is in no list to walk.
    s.bind(
        "th_list",
        "arrayMap(t -> t.1, arraySort(t -> (t.2, -toInt64(t.3), -toInt64(t.1)), \
         arrayFilter(t -> t.2 > 0 AND bitAnd(st_mflags[t.1], 8) = 0, th_all)))",
    );

    // The view point's own sine and cosine turn a thing's position into a
    // distance along the view and an offset across it.
    s.bind("v_cos", "k_finesine[bitShiftRight(v_angle, 19) + 2049]");
    s.bind("v_sin", "k_finesine[bitShiftRight(v_angle, 19) + 1]");
    s.bind(
        "sp_trx",
        "arrayMap(i -> toInt32(toInt64(st_mx[i]) - toInt64(v_x)), th_list)",
    );
    s.bind(
        "sp_try",
        "arrayMap(i -> toInt32(toInt64(st_my[i]) - toInt64(v_y)), th_list)",
    );
    s.bind(
        "sp_tz",
        format!(
            "arrayMap((rx, ry) -> toInt32(toInt64({}) + toInt64({})), sp_trx, sp_try)",
            fixed::fixed_mul("rx", "v_cos"),
            fixed::fixed_mul("ry", "v_sin")
        ),
    );
    s.bind(
        "sp_tx",
        format!(
            "arrayMap((rx, ry) -> toInt32(toInt64({}) - toInt64({})), sp_trx, sp_try)",
            fixed::fixed_mul("rx", "v_sin"),
            fixed::fixed_mul("ry", "v_cos")
        ),
    );
    s.bind(
        "sp_xscale",
        format!(
            "arrayMap(z -> if(z < 262144, toInt32(0), {}), sp_tz)",
            fixed::fixed_div(&CENTER_X_FRAC.to_string(), "z")
        ),
    );

    // Which of the eight pictures of the frame faces the view point.
    s.bind(
        "sp_ang",
        format!(
            "arrayMap((rx, ry) -> {}, sp_trx, sp_try)",
            fixed::point_to_angle("rx", "ry", "k_tantoangle")
        ),
    );
    s.bind(
        "sp_rot",
        "arrayMap((a, i) -> bitShiftRight(toUInt32(toUInt64(a) + 4294967296 \
         - toUInt64(st_mangle[i]) + 2415919104), 29), sp_ang, th_list)",
    );
    s.bind(
        "sp_slot",
        "arrayMap(i -> toUInt32((st_msprite[i] * 32 + bitAnd(st_mframe[i], 32767)) * 8), th_list)",
    );
    s.bind(
        "sp_lump",
        "arrayMap((sl, r) -> k_spr_lump[sl + if(k_spr_rotate[sl + 1] = 1, toUInt32(r), \
         toUInt32(0)) + 1], sp_slot, sp_rot)",
    );
    s.bind(
        "sp_flip",
        "arrayMap((sl, r) -> k_spr_flip[sl + if(k_spr_rotate[sl + 1] = 1, toUInt32(r), \
         toUInt32(0)) + 1], sp_slot, sp_rot)",
    );

    // The two screen columns the picture spans.
    s.bind(
        "sp_txl",
        "arrayMap((tx, lp) -> toInt32(toInt64(tx) - toInt64(k_spl_left[lp + 1])), sp_tx, sp_lump)",
    );
    s.bind(
        "sp_txr",
        "arrayMap((tx, lp) -> toInt32(toInt64(tx) + toInt64(k_spl_widthf[lp + 1])), \
         sp_txl, sp_lump)",
    );
    s.bind(
        "sp_x1",
        format!(
            "arrayMap((tx, xs) -> bitShiftRight(toInt32({CENTER_X_FRAC} + toInt64({})), 16), \
             sp_txl, sp_xscale)",
            fixed::fixed_mul("tx", "xs")
        ),
    );
    s.bind(
        "sp_x2",
        format!(
            "arrayMap((tx, xs) -> bitShiftRight(toInt32({CENTER_X_FRAC} + toInt64({})), 16) - 1, \
             sp_txr, sp_xscale)",
            fixed::fixed_mul("tx", "xs")
        ),
    );
    s.bind(
        "sp_ok",
        format!(
            "arrayMap((z, tx, x1, x2, lp) -> toUInt8(z >= 262144 \
             AND toInt32(abs(toInt64(tx))) <= toInt32(bitShiftLeft(toInt64(z), 2)) \
             AND x1 <= {VIEW_WIDTH} AND x2 >= 0 AND lp >= 0), \
             sp_tz, sp_tx, sp_x1, sp_x2, sp_lump)"
        ),
    );

    // The rest of the vissprite.
    s.bind(
        "sp_gzt",
        "arrayMap((i, lp) -> toInt32(toInt64(st_mz[i]) + toInt64(k_spl_top[lp + 1])), \
         th_list, sp_lump)",
    );
    s.bind(
        "sp_mid",
        "arrayMap(g -> toInt32(toInt64(g) - toInt64(v_z)), sp_gzt)",
    );
    s.bind("sp_vx1", "arrayMap(x -> greatest(x, 0), sp_x1)");
    s.bind(
        "sp_vx2",
        format!("arrayMap(x -> least(x, {}), sp_x2)", VIEW_WIDTH - 1),
    );
    // A thing behind the view plane has no scale, and nothing divides by it:
    // `R_ProjectSprite` returns before it gets here.
    s.bind(
        "sp_xiscale",
        format!(
            "arrayMap((f, xs) -> if(xs = 0, toInt32(0), \
             if(f = 1, toInt32(-toInt64({d})), {d})), sp_flip, sp_xscale)",
            d = fixed::fixed_div("65536", "greatest(xs, toInt32(1))")
        ),
    );
    s.bind(
        "sp_frac0",
        "arrayMap((f, lp, xi, vx1, x1) -> toInt32(\
         if(f = 1, toInt64(k_spl_widthf[lp + 1]) - 1, toInt64(0)) \
         + if(vx1 > x1, toInt64(xi) * toInt64(vx1 - x1), toInt64(0))), \
         sp_flip, sp_lump, sp_xiscale, sp_vx1, sp_x1)",
    );
    // The light a thing takes is its own sector's. A thing that draws as a
    // shadow takes -1, which nothing here draws.
    s.bind(
        "sp_light",
        "arrayMap(i -> toUInt32(least(greatest(toInt64(bitShiftRight(\
         st_seclight[k_ssec_sector[st_msubsector[i] + 1] + 1], 4)) \
         + toInt64(v_extralight), 0), 15)), th_list)",
    );
    s.bind(
        "sp_cmap",
        "arrayMap((i, xs, li) -> multiIf(\
         bitAnd(st_mflags[i], 262144) != 0, toInt32(-1), \
         v_fixedcolormap != 0, v_fixedcolormap, \
         bitAnd(st_mframe[i], 32768) != 0, toInt32(0), \
         toInt32(k_scalelight[li * 48 + toUInt32(least(bitShiftRight(xs, 12), 47)) + 1])), \
         th_list, sp_xscale, sp_light)",
    );
    s.bind(
        "sp_sprtop",
        format!(
            "arrayMap((md, xs) -> toInt32({} - toInt64({})), sp_mid, sp_xscale)",
            CENTER_Y << 16,
            fixed::fixed_mul("md", "xs")
        ),
    );

    // Sorted by scale, smallest first, ties in the order they were added,
    // which is where the engine's own selection sort leaves them.
    s.bind(
        "vs_all",
        "arrayFilter(t -> t.1 = 1, arrayZip(sp_ok, sp_xscale, sp_vx1, sp_vx2, sp_mid, \
         sp_frac0, sp_xiscale, sp_lump, sp_cmap, sp_sprtop, \
         arrayMap(i -> st_mx[i], th_list), arrayMap(i -> st_my[i], th_list), \
         arrayMap(i -> st_mz[i], th_list), sp_gzt, \
         arrayMap(e -> toUInt32(e), arrayEnumerate(sp_ok))))",
    );
    s.bind("vs", "arraySort(t -> (t.2, t.15), vs_all)");
    // A thing drawn as a shadow reads the framebuffer under it, which is a
    // frame this does not draw.
    s.bind(
        "vs_shadow",
        "countEqual(arrayMap(t -> t.9, vs), toInt32(-1))",
    );
}

// ---------------------------------------------------------------------------
// R_DrawSprite's clipping
// ---------------------------------------------------------------------------

/// What each drawseg leaves of each sprite.
///
/// The drawsegs are scanned from the last one back. A drawseg in front of the
/// sprite that carries a silhouette clips it, and the first one to reach a
/// column is the one that decides it. A drawseg behind the sprite decides
/// nothing.
fn sprite_clip(s: &mut Stage) {
    // The clips as they stood after each drawseg wrote its column, which is
    // what `R_StoreWallRange` copies into the drawseg.
    let post_cc = format!(
        "multiIf({mid} != 0, {VIEW_HEIGHT}, {top} != 0, if({midlo} >= {yl}, {midlo}, {yl} - 1), \
         {mkc} = 1, {yl} - 1, {cc0})",
        mid = cw("midtex"),
        top = cw("toptex"),
        midlo = cw("midlo"),
        yl = cw("yl"),
        mkc = cw("mkc"),
        cc0 = cw("cc0"),
    );
    let post_fc = format!(
        "multiIf({mid} != 0, -1, {bot} != 0, if({botlo} <= {yh}, {botlo}, {yh} + 1), \
         {mkf} = 1, {yh} + 1, {fc0})",
        mid = cw("midtex"),
        bot = cw("bottex"),
        botlo = cw("botlo"),
        yh = cw("yh"),
        mkf = cw("mkf"),
        fc0 = cw("fc0"),
    );
    s.bind(
        "dsclip",
        format!(
            "arraySort(t -> t.1, arrayMap(t -> (toUInt32({ds}) * 512 + toUInt32({x}), \
             toInt32({post_cc}), toInt32({post_fc})), cw))",
            ds = cw("ds"),
            x = cw("x"),
        ),
    );
    // Where each drawseg's run of columns starts in that sort. A drawseg
    // covers every column between its two ends, so the run is its width.
    s.bind(
        "ds_clipbase",
        shifted(
            "arrayCumSum(arrayMap((a, b) -> toUInt64(b - a + 1), ds_x1, ds_x2))",
            "toUInt64(0)",
            true,
        ),
    );

    // One row per drawseg, with everything `R_DrawSprite` reads off it.
    s.bind(
        "dsq",
        "arrayMap(i -> (toUInt32(i), ds_x1[i], ds_x2[i], w_sil[i], w_tsil[i], w_bsil[i], \
         w_masked[i], greatest(w_scale1[i], w_scale2[i]), least(w_scale1[i], w_scale2[i]), \
         w_topconst[i], w_botconst[i], w_seg[i]), arrayEnumerate(ds_qi))",
    );
    // A drawseg is behind the sprite when it is smaller at both ends, or
    // smaller at one end with the sprite on its front side.
    let behind = format!(
        "d.8 < v.2 OR (d.9 < v.2 AND {} = 0)",
        fixed::point_on_side(
            "v.11",
            "v.12",
            "k_seg_v1x[d.12 + 1]",
            "k_seg_v1y[d.12 + 1]",
            "toInt32(toInt64(k_seg_v2x[d.12 + 1]) - toInt64(k_seg_v1x[d.12 + 1]))",
            "toInt32(toInt64(k_seg_v2y[d.12 + 1]) - toInt64(k_seg_v1y[d.12 + 1]))",
            16
        )
    );
    s.bind(
        "sp_q",
        format!(
            "arrayMap(v -> arrayReverse(arrayFilter(d -> \
             d.2 <= v.4 AND d.3 >= v.3 AND (d.4 != 0 OR d.7 = 1) AND NOT ({behind}), dsq)), vs)"
        ),
    );

    // One row per sprite column: the two clips, and everything the pixels
    // read off the sprite.
    let bottom = "arrayMap(b -> if(b.11 = 1, -1, \
                  dsclip[ds_clipbase[b.1] + toUInt64(x - b.2) + 1].3), \
                  arraySlice(arrayFilter(d -> x >= d.2 AND x <= d.3 \
                  AND bitAnd(d.4, 1) != 0 AND v.13 < d.6, q), 1, 1))";
    let top = format!(
        "arrayMap(b -> if(b.10 = 1, {VIEW_HEIGHT}, \
         dsclip[ds_clipbase[b.1] + toUInt64(x - b.2) + 1].2), \
         arraySlice(arrayFilter(d -> x >= d.2 AND x <= d.3 \
         AND bitAnd(d.4, 2) != 0 AND v.14 > d.5, q), 1, 1))"
    );
    s.bind(
        "sp_cols",
        format!(
            "arrayFlatten(arrayMap((v, q, k) -> arrayMap(x -> (\
             toUInt32(k), toInt32(x), \
             arrayPushBack({bottom}, toInt32({VIEW_HEIGHT}))[1], \
             arrayPushBack({top}, toInt32(-1))[1], \
             bitShiftRight(toInt32(toInt64(v.6) + toInt64(v.7) * toInt64(x - v.3)), 16), \
             v.8, v.5, v.2, toInt32(abs(toInt64(v.7))), v.9, v.10), \
             range(v.3, v.4 + 1)), vs, sp_q, arrayEnumerate(vs)))"
        ),
    );
}

// ---------------------------------------------------------------------------
// R_DrawVisSprite, R_DrawMaskedColumn and R_DrawPlayerSprites
// ---------------------------------------------------------------------------

/// The pixels of every sprite, each carrying the order it was drawn in so a
/// nearer sprite covers a farther one.
fn sprite_pixels(s: &mut Stage) {
    s.bind("sp_lit", "arrayFilter(c -> c.10 >= 0, sp_cols)");
    s.bind(
        "sp_px",
        format!(
            "arrayFlatten(arrayMap(c -> {}, sp_lit))",
            masked_column("c", "toUInt64(c.1)")
        ),
    );
}

/// The player's own two sprites, drawn over everything at a fixed scale and
/// clipped only by the edges of the view.
fn psprites(s: &mut Stage) {
    s.state("st_psp_state", "psp_state");
    s.state("st_psp_sx", "psp_sx");
    s.state("st_psp_sy", "psp_sy");
    s.state("st_powers", "p_powers");

    s.bind("ps_slot", "arrayFilter(i -> st_psp_state[i] >= 0, [1, 2])");
    s.bind(
        "ps_frame",
        "arrayMap(i -> toUInt32((k_state_sprite[st_psp_state[i] + 1] * 32 \
         + bitAnd(k_state_frame[st_psp_state[i] + 1], 32767)) * 8), ps_slot)",
    );
    s.bind("ps_lump", "arrayMap(f -> k_spr_lump[f + 1], ps_frame)");
    s.bind("ps_flip", "arrayMap(f -> k_spr_flip[f + 1], ps_frame)");
    s.bind(
        "ps_txl",
        "arrayMap((i, lp) -> toInt32(toInt64(st_psp_sx[i]) - 10485760 \
         - toInt64(k_spl_left[lp + 1])), ps_slot, ps_lump)",
    );
    s.bind(
        "ps_x1",
        format!(
            "arrayMap(tx -> bitShiftRight(toInt32({CENTER_X_FRAC} + toInt64({})), 16), ps_txl)",
            fixed::fixed_mul("tx", "65536")
        ),
    );
    s.bind(
        "ps_x2",
        format!(
            "arrayMap((tx, lp) -> bitShiftRight(toInt32({CENTER_X_FRAC} + toInt64({})), 16) - 1, \
             ps_txl, ps_lump)",
            fixed::fixed_mul(
                "toInt32(toInt64(tx) + toInt64(k_spl_widthf[lp + 1]))",
                "65536"
            )
        ),
    );
    s.bind(
        "ps_ok",
        format!(
            "arrayMap((x1, x2, lp) -> toUInt8(x1 <= {VIEW_WIDTH} AND x2 >= 0 AND lp >= 0), \
             ps_x1, ps_x2, ps_lump)"
        ),
    );
    // `BASEYCENTER` is 100, and half a unit is added the way the C adds it.
    s.bind(
        "ps_mid",
        "arrayMap((i, lp) -> toInt32(6553600 + 32768 \
         - (toInt64(st_psp_sy[i]) - toInt64(k_spl_top[lp + 1]))), ps_slot, ps_lump)",
    );
    s.bind("ps_vx1", "arrayMap(x -> greatest(x, 0), ps_x1)");
    s.bind(
        "ps_vx2",
        format!("arrayMap(x -> least(x, {}), ps_x2)", VIEW_WIDTH - 1),
    );
    s.bind(
        "ps_xiscale",
        "arrayMap(f -> if(f = 1, toInt32(-65536), toInt32(65536)), ps_flip)",
    );
    s.bind(
        "ps_frac0",
        "arrayMap((f, lp, xi, vx1, x1) -> toInt32(\
         if(f = 1, toInt64(k_spl_widthf[lp + 1]) - 1, toInt64(0)) \
         + if(vx1 > x1, toInt64(xi) * toInt64(vx1 - x1), toInt64(0))), \
         ps_flip, ps_lump, ps_xiscale, ps_vx1, ps_x1)",
    );
    // The light is the player's own sector's, at the brightest scale.
    s.bind(
        "ps_light",
        "toUInt32(least(greatest(toInt64(bitShiftRight(\
         st_seclight[k_ssec_sector[st_msubsector[st_mo] + 1] + 1], 4)) \
         + toInt64(v_extralight), 0), 15))",
    );
    s.bind(
        "ps_cmap",
        "arrayMap(i -> multiIf(\
         st_powers[3] > 128 OR bitAnd(st_powers[3], 8) != 0, toInt32(-1), \
         v_fixedcolormap != 0, v_fixedcolormap, \
         bitAnd(k_state_frame[st_psp_state[i] + 1], 32768) != 0, toInt32(0), \
         toInt32(k_scalelight[ps_light * 48 + 48])), ps_slot)",
    );
    s.bind(
        "ps_sprtop",
        format!(
            "arrayMap(md -> toInt32({} - toInt64({})), ps_mid)",
            CENTER_Y << 16,
            fixed::fixed_mul("md", "65536")
        ),
    );
    // The same column rows a sprite makes, at the fixed scale and clipped by
    // the view's own edges.
    s.bind(
        "ps_cols",
        format!(
            "arrayFlatten(arrayMap((k, x1, x2, f0, xi, lp, md, cm, tp) -> arrayMap(x -> (\
             toUInt32(1048000 + k), toInt32(x), toInt32({VIEW_HEIGHT}), toInt32(-1), \
             bitShiftRight(toInt32(toInt64(f0) + toInt64(xi) * toInt64(x - x1)), 16), \
             lp, md, toInt32(65536), toInt32(abs(toInt64(xi))), cm, tp), \
             range(x1, x2 + 1)), \
             arrayMap(e -> toUInt32(e), arrayEnumerate(ps_slot)), \
             ps_vx1, ps_vx2, ps_frac0, ps_xiscale, ps_lump, ps_mid, ps_cmap, ps_sprtop))"
        ),
    );
    s.bind(
        "ps_visible",
        "arrayFilter((c, k) -> ps_ok[k] = 1, ps_cols, arrayMap(c -> c.1 - 1048000, ps_cols))",
    );
    s.bind("ps_lit", "arrayFilter(c -> c.10 >= 0, ps_visible)");
    s.bind(
        "ps_px",
        format!(
            "arrayFlatten(arrayMap(c -> {}, ps_lit))",
            masked_column("c", "toUInt64(c.1)")
        ),
    );
}

/// One masked column: every post of it, clipped against the two clip rows and
/// drawn. `c` names the column row and `time` the order it is drawn in.
fn masked_column(c: &str, time: &str) -> String {
    let slot = format!("toUInt32({c}.6) * 256 + toUInt32({c}.5)");
    let frac = format!(
        "toUInt32(toInt64({c}.7) - bitShiftLeft(toInt64(td), 16) \
         + toInt64(y - {CENTER_Y}) * toInt64({c}.9))"
    );
    let texel = pool(
        "k_sprpool",
        &format!("of + toUInt32(bitAnd(bitShiftRight({frac}, 16), 127))"),
    );
    let cmap = pool("k_colormap", &format!("toUInt32({c}.10) * 256 + {texel}"));
    let rows = format!(
        "arrayMap(y -> (toUInt64(y * {VIEW_WIDTH} + {c}.2) * 1048576 + {time}, {cmap}), \
         range(yl, greatest(yh + 1, yl)))"
    );
    let clipped = let_in(
        &[
            (
                "yl",
                format!("greatest(bitShiftRight(toInt32(toInt64(ts) + 65535), 16), {c}.4 + 1)"),
            ),
            (
                "yh",
                format!(
                    "least(bitShiftRight(toInt32(toInt64(ts) \
                     + toInt64({c}.8) * toInt64(ln) - 1), 16), {c}.3 - 1)"
                ),
            ),
        ],
        &rows,
    );
    let with_top = let_in(
        &[(
            "ts",
            format!("toInt32(toInt64({c}.11) + toInt64({c}.8) * toInt64(td))"),
        )],
        &clipped,
    );
    let with_post = let_in(
        &[
            ("td", "toInt32(k_spost_top[p + 1])".to_owned()),
            ("ln", "toInt32(k_spost_len[p + 1])".to_owned()),
            ("of", "k_spost_ofs[p + 1]".to_owned()),
        ],
        &with_top,
    );
    format!(
        "arrayFlatten(arrayMap(p -> {with_post}, \
         range(k_spost_first[{slot} + 1], \
         k_spost_first[{slot} + 1] + k_spost_num[{slot} + 1])))"
    )
}

// ---------------------------------------------------------------------------
// HU_Drawer
// ---------------------------------------------------------------------------

/// The message across the top of the view.
///
/// The state row names the line the message widget is showing by the hash of
/// its bytes, so the text comes out of the table of everything `d_englsh.h`
/// defines. `HUlib_drawTextLine` upper-cases each letter, draws the font
/// patch for it and steps by that patch's width; a space, or a character the
/// font has no patch for, steps four and draws nothing.
fn message(s: &mut Stage) {
    s.state("st_hu_on", "hu_message_on");
    s.state("st_hu_message", "hu_message");

    // The column holds the hash as digits, because the reference emulator's
    // probe has a pointer to hash and nowhere to put the bytes.
    s.bind("hu_hash", "toUInt64OrZero(st_hu_message)");
    s.bind(
        "hu_text",
        "if(st_hu_on = 0, '', \
         arrayFirst(t -> 1, arrayPushBack(arrayMap(t -> t.2, \
         arrayFilter(t -> t.1 = hu_hash, arrayZip(k_msg_hash, k_msg_text))), '')))",
    );
    // A message the table does not carry would draw as nothing where the
    // engine drew words.
    s.bind(
        "hu_guard",
        "throwIf(st_hu_on = 1 AND hu_hash != 0 AND empty(hu_text), \
         'the state names a message the table does not carry')",
    );
    // One entry per character: the font patch it draws, and how far along the
    // line it starts. The step depends on every character before it, so the
    // line is walked once.
    s.bind(
        "hu_chars",
        "arrayMap(c -> upperUTF8(c), \
         arrayMap(i -> substring(hu_text, i, 1), range(1, length(hu_text) + 1)))",
    );
    s.bind(
        "hu_code",
        "arrayMap(c -> toInt32(reinterpretAsUInt8(c)), hu_chars)",
    );
    s.bind(
        "hu_step",
        "arrayMap(c -> if(c != 32 AND c >= 33 AND c <= 95, \
         toInt32(k_ui_width[k_ui_slot[77 + c - 33 + 1] + 1]), toInt32(4)), hu_code)",
    );
    s.bind(
        "hu_x",
        shifted(
            "arrayMap(x -> toInt32(x), arrayCumSum(hu_step))",
            "toInt32(0)",
            true,
        ),
    );
    // The line stops at the first character that would not fit.
    s.bind(
        "hu_fit",
        format!(
            "arrayMap((c, x, w) -> toUInt8(if(c != 32 AND c >= 33 AND c <= 95, \
             x + w <= {VIEW_WIDTH}, x + w < {VIEW_WIDTH})), hu_code, hu_x, hu_step)"
        ),
    );
    s.bind(
        "hu_drawn",
        "arrayFilter((t, k) -> k <= arrayFirstIndex(f -> f = 0, arrayPushBack(hu_fit, toUInt8(0))) - 1 \
         AND t.1 != 32 AND t.1 >= 33 AND t.1 <= 95, \
         arrayZip(hu_code, hu_x), arrayEnumerate(hu_code))",
    );
    // `V_DrawPatch` takes the patch's own offsets off the corner it is asked
    // to draw at.
    s.bind(
        "hu_patch",
        format!(
            "arrayMap(t -> (toUInt32(k_ui_slot[77 + t.1 - 33 + 1]), t.2, toInt32(0), \
             toUInt64({})), hu_drawn)",
            MESSAGE_TIME
        ),
    );
    s.bind(
        "hu_px",
        format!("arrayFlatten(arrayMap(b -> {}, hu_patch))", ui_blit("b")),
    );
}

/// One `V_DrawPatch`: every post of every column of the patch, straight into
/// the frame with no colormap. `b` names a row of `(patch, x, y, time)`, with
/// `x` and `y` the corner the engine asks for, which the patch's own offsets
/// come off before anything is drawn.
fn ui_blit(b: &str) -> String {
    let slot = format!("toUInt32({b}.1) * 512 + toUInt32(col)");
    let texel = pool(
        "k_uipool",
        &format!(
            "k_uipost_ofs[p + 1] \
             + toUInt32(y - {b}.3 + toInt32(k_ui_top[{b}.1 + 1]) - toInt32(k_uipost_top[p + 1]))"
        ),
    );
    let left = format!("({b}.2 - toInt32(k_ui_left[{b}.1 + 1]))");
    let top = format!("({b}.3 - toInt32(k_ui_top[{b}.1 + 1]) + toInt32(k_uipost_top[p + 1]))");
    let rows = format!(
        "arrayMap(y -> (toUInt64(y * {VIEW_WIDTH} + {left} + col) * 1048576 + {b}.4, {texel}), \
         range({top}, {top} + toInt32(k_uipost_len[p + 1])))"
    );
    let posts = format!(
        "arrayFlatten(arrayMap(p -> {rows}, \
         range(k_uipost_first[{slot} + 1], \
         k_uipost_first[{slot} + 1] + k_uipost_num[{slot} + 1])))"
    );
    format!(
        "arrayFlatten(arrayMap(col -> {posts}, \
         range(toUInt32(k_ui_width[{b}.1 + 1]))))"
    )
}

// ---------------------------------------------------------------------------
// ST_Drawer
// ---------------------------------------------------------------------------

/// The status bar.
///
/// `ST_drawWidgets` runs every frame with `refresh` false, because nothing a
/// demo does makes the engine ask for a full one. A number therefore copies
/// the bare status bar back over its own area and draws its digits again,
/// every frame. A percent sign is only drawn on a full refresh, so it stays
/// where the frame before left it. An icon is only drawn when its value
/// changed, which is what `st_cache` carries from frame to frame.
///
/// The layout is `ST_createWidgets`'s, written out because the engine only
/// ever builds these widgets at these places.
fn status_bar(s: &mut Stage) {
    s.state("st_health", "p_health");
    s.state("st_armorpoints", "p_armorpoints");
    s.state("st_ammo", "p_ammo");
    s.state("st_maxammo", "p_maxammo");
    s.state("st_readyweapon", "p_readyweapon");
    s.state("st_weaponowned", "p_weaponowned");
    s.state("st_cards", "p_cards");
    s.state("st_face", "st_faceindex");
    s.bind(
        "prev_cache",
        format!(
            "joinGet('{}.native_frames', 'st_cache', toUInt32(frame - 1))",
            s.db
        ),
    );

    // `w_ready` reads the ammo the ready weapon takes. A weapon that takes
    // none reads 1994, which clears the area and draws nothing.
    s.bind(
        "sb_ready",
        "if(k_weapon_ammo[st_readyweapon + 1] >= 4, toInt32(1994), \
         st_ammo[k_weapon_ammo[st_readyweapon + 1] + 1])",
    );
    // `(x, y, digits, first patch slot, value)`, in `ST_drawWidgets`'s order:
    // the ready ammo, then each kind of ammo beside the most of it the player
    // can carry, then health and armour.
    s.bind(
        "sb_num",
        "arrayConcat(\
         [(toInt32(44), toInt32(171), toInt32(3), toUInt32(0), sb_ready)], \
         arrayFlatten(arrayMap(i -> [\
           (toInt32(288), toInt32([173, 179, 191, 185][i + 1]), toInt32(3), toUInt32(10), \
            st_ammo[i + 1]), \
           (toInt32(314), toInt32([173, 179, 191, 185][i + 1]), toInt32(3), toUInt32(10), \
            st_maxammo[i + 1])], range(4))), \
         [(toInt32(90), toInt32(171), toInt32(3), toUInt32(0), st_health), \
          (toInt32(221), toInt32(171), toInt32(3), toUInt32(0), st_armorpoints)])",
    );
    // A negative number draws a minus sign, which no widget on this status
    // bar can reach: the frags counter is the only one that goes below zero
    // and it is off outside deathmatch.
    s.bind(
        "sb_guard",
        "throwIf(arrayExists(w -> w.5 < 0, sb_num), \
         'a status bar number is negative, which draws a minus sign')",
    );
    // The area each number clears: as many digits wide as the widget holds,
    // one digit tall, ending at the widget's own x.
    s.bind(
        "sb_clear",
        format!(
            "arrayMap((w, k) -> (w.1 - w.3 * toInt32(k_ui_width[k_ui_slot[w.4 + 1] + 1]), w.2, \
             w.3 * toInt32(k_ui_width[k_ui_slot[w.4 + 1] + 1]), \
             toInt32(k_ui_height[k_ui_slot[w.4 + 1] + 1]), \
             toUInt64({STATUS_TIME}) + toUInt64(k) * 2), sb_num, arrayEnumerate(sb_num))"
        ),
    );
    // The digits, from the right. Zero draws one digit; anything else draws
    // as many as it has, up to what the widget holds.
    s.bind(
        "sb_digits",
        format!(
            "arrayFlatten(arrayMap((w, k) -> if(w.5 = 1994, \
             CAST([], 'Array(Tuple(UInt32, Int32, Int32, UInt64))'), \
             arrayMap(d -> (\
               toUInt32(k_ui_slot[w.4 + intDiv(w.5, toInt32(pow(10, d))) % 10 + 1]), \
               w.1 - (d + 1) * toInt32(k_ui_width[k_ui_slot[w.4 + 1] + 1]), w.2, \
               toUInt64({STATUS_TIME}) + toUInt64(k) * 2 + 1), \
             if(w.5 = 0, [toInt32(0)], \
                arrayFilter(d -> intDiv(w.5, toInt32(pow(10, d))) > 0, \
                            arrayMap(e -> toInt32(e), range(toUInt32(w.3))))))), \
             sb_num, arrayEnumerate(sb_num)))"
        ),
    );

    // The icons. Each carries what it drew last frame, so one whose value did
    // not move is left where it is. `(patch slot base, now, before, x, y,
    // kind)`, where kind 0 is a weapon number, 1 the arms background and 2 a
    // plain icon.
    s.bind(
        "sb_keybox",
        "arrayMap(i -> toInt32(multiIf(st_cards[i + 4] = 1, i + 3, st_cards[i + 1] = 1, i, -1)), \
         range(3))",
    );
    s.bind(
        "sb_icon",
        "arrayConcat(\
         [(toUInt32(27), toInt32(1), prev_cache.10, toInt32(104), toInt32(168), toUInt8(1), \
           toUInt32(0))], \
         arrayMap(i -> (toUInt32(0), toInt32(st_weaponowned[i + 2]), prev_cache.7[i + 1], \
           toInt32(111 + (i % 3) * 12), toInt32(172 + intDiv(i, 3) * 10), toUInt8(0), \
           toUInt32(i)), range(6)), \
         [(toUInt32(34), st_face, prev_cache.9, toInt32(143), toInt32(168), toUInt8(2), \
           toUInt32(0))], \
         arrayMap(i -> (toUInt32(21), sb_keybox[i + 1], prev_cache.8[i + 1], \
           toInt32(239), toInt32(171 + i * 10), toUInt8(2), toUInt32(i)), range(3)))",
    );
    // A weapon number is grey when the weapon is not owned and the short
    // yellow digit when it is, which is the one icon two patch sets serve.
    // The arms background is a yes or no rather than a picture number.
    let which = |value: &str| {
        format!(
            "arrayMap(ic -> multiIf(\
             ic.6 = 0, if({value} = 0, toUInt32(28) + ic.7, toUInt32(12) + ic.7), \
             ic.6 = 1, ic.1, \
             ic.1 + toUInt32(greatest({value}, 0))), sb_icon)"
        )
    };
    s.bind("sb_icon_now", which("ic.2"));
    s.bind("sb_icon_was", which("ic.3"));
    // The arms background is a yes or no rather than a picture number: when
    // it turns off, its own area goes back instead.
    s.bind(
        "sb_icon_moved",
        "arrayMap(ic -> toUInt8(ic.2 != ic.3 AND (ic.6 = 1 OR ic.2 != -1)), sb_icon)",
    );
    // A picture that turns off puts its own area back; one that turns on
    // draws over whatever was there.
    s.bind(
        "sb_icon_off",
        "arrayMap(ic -> toUInt8(if(ic.6 = 1, ic.2 = 0, ic.3 != -1)), sb_icon)",
    );
    s.bind(
        "sb_icon_draw",
        format!(
            "arrayFilter((t, m, ic) -> m = 1 AND (ic.6 != 1 OR ic.2 != 0), \
             arrayMap((ic, p, k) -> (toUInt32(k_ui_slot[p + 1]), ic.4, ic.5, \
             toUInt64({STATUS_TIME}) + 1000 + toUInt64(k) * 2 + 1), \
             sb_icon, sb_icon_now, arrayEnumerate(sb_icon)), sb_icon_moved, sb_icon)"
        ),
    );
    // What the old picture covered, put back before the new one goes down.
    s.bind(
        "sb_icon_clear",
        format!(
            "arrayFilter((t, m, off) -> m = 1 AND off = 1, \
             arrayMap((ic, p, k) -> (\
             ic.4 - toInt32(k_ui_left[k_ui_slot[p + 1] + 1]), \
             ic.5 - toInt32(k_ui_top[k_ui_slot[p + 1] + 1]), \
             toInt32(k_ui_width[k_ui_slot[p + 1] + 1]), \
             toInt32(k_ui_height[k_ui_slot[p + 1] + 1]), \
             toUInt64({STATUS_TIME}) + 1000 + toUInt64(k) * 2), \
             sb_icon, sb_icon_was, arrayEnumerate(sb_icon)), sb_icon_moved, sb_icon_off)"
        ),
    );
    s.bind(
        "sb_px",
        format!(
            "arrayConcat(\
             arrayFlatten(arrayMap(r -> {restore}, sb_clear)), \
             arrayFlatten(arrayMap(b -> {blit}, sb_digits)), \
             arrayFlatten(arrayMap(r -> {restore}, sb_icon_clear)), \
             arrayFlatten(arrayMap(b -> {blit}, sb_icon_draw)))",
            restore = ui_restore("r"),
            blit = ui_blit("b"),
        ),
    );
}

/// `V_CopyRect` out of the bare status bar: the rectangle `r` names, put back
/// where it came from.
fn ui_restore(r: &str) -> String {
    let backing = pool(
        "k_ui_backing",
        &format!("({r}.2 + dy - {STATUS_BAR_Y}) * {VIEW_WIDTH} + {r}.1 + dx"),
    );
    format!(
        "arrayFlatten(arrayMap(dy -> arrayMap(dx -> (\
         toUInt64(({r}.2 + dy) * {VIEW_WIDTH} + {r}.1 + dx) * 1048576 + {r}.5, \
         {backing}), \
         range(toUInt32({r}.3))), range(toUInt32({r}.4))))"
    )
}

// ---------------------------------------------------------------------------
// R_DrawFuzzColumn
// ---------------------------------------------------------------------------

/// A thing drawn as a shadow.
///
/// `R_DrawFuzzColumn` never reads the picture. It reads the frame it is
/// drawing into, one row above or below the pixel it is about to write, and
/// puts that colour back through colormap 6. Which of the two it reads is
/// `fuzzoffset[fuzzpos]`, and `fuzzpos` steps once per pixel drawn and carries
/// on into the next frame.
///
/// Reading below is always the frame as it stood before the shadow started,
/// because a column draws downwards. Reading above is that same frame unless
/// the row above is one this column has just written, so each column is walked
/// in one fold carrying the row it last wrote and the colour it put there.
fn fuzz(s: &mut Stage) {
    s.table("k_fuzzoffset", "fuzzoffset", "value", "id");
    s.bind(
        "prev_fuzzpos",
        format!(
            "coalesce(joinGetOrNull('{}.native_frames', 'fuzzpos', toUInt32(frame - 1)), \
             toUInt8(0))",
            s.db
        ),
    );
    s.bind(
        "fz_cols",
        "arrayFilter(c -> c.10 < 0, arrayConcat(sp_cols, ps_visible))",
    );
    // Every shadow on screen shares one walk of `fuzzpos` and they are drawn
    // one after another, so two of them would each need the frame as it stood
    // when it began.
    s.bind(
        "fz_guard",
        "throwIf(length(arrayDistinct(arrayMap(c -> c.1, fz_cols))) > 1, \
         'more than one thing draws as a shadow in one frame')",
    );
    s.bind(
        "fz_time",
        "arrayReduce('min', arrayPushBack(arrayMap(c -> c.1, fz_cols), toUInt32(4294967295)))",
    );

    // The rows each column writes, in the order it writes them.
    // `R_DrawFuzzColumn` pulls the two ends one row inside the view before it
    // starts, because it reads a row past each of them.
    let post_yl = "greatest(bitShiftRight(toInt32(toInt64(ts) + 65535), 16), c.4 + 1)";
    let post_yh = "least(bitShiftRight(toInt32(toInt64(ts) \
                   + toInt64(c.8) * toInt64(ln) - 1), 16), c.3 - 1)";
    let clamped = let_in(
        &[
            ("fyl", format!("if({post_yl} = 0, 1, {post_yl})")),
            (
                "fyh",
                format!(
                    "if({post_yh} = {last}, {last} - 1, {post_yh})",
                    last = VIEW_HEIGHT - 1
                ),
            ),
        ],
        "arrayMap(y -> toInt32(y), range(fyl, greatest(fyh + 1, fyl)))",
    );
    let with_top = let_in(
        &[(
            "ts",
            "toInt32(toInt64(c.11) + toInt64(c.8) * toInt64(td))".to_owned(),
        )],
        &clamped,
    );
    let with_post = let_in(
        &[
            ("td", "toInt32(k_spost_top[p + 1])".to_owned()),
            ("ln", "toInt32(k_spost_len[p + 1])".to_owned()),
        ],
        &with_top,
    );
    let slot = "toUInt32(c.6) * 256 + toUInt32(c.5)";
    s.bind(
        "fz_rows",
        format!(
            "arrayMap(c -> arrayFlatten(arrayMap(p -> {with_post}, \
             range(k_spost_first[{slot} + 1], \
             k_spost_first[{slot} + 1] + k_spost_num[{slot} + 1]))), fz_cols)"
        ),
    );
    s.bind(
        "fz_start",
        shifted(
            "arrayCumSum(arrayMap(r -> toUInt64(length(r)), fz_rows))",
            "toUInt64(0)",
            true,
        ),
    );

    // The corner of the frame the shadow reads, one row past each end of it.
    s.bind("fz_x0", "arrayReduce('min', arrayMap(c -> c.2, fz_cols))");
    s.bind("fz_x1", "arrayReduce('max', arrayMap(c -> c.2, fz_cols))");
    s.bind("fz_w", "fz_x1 - fz_x0 + 1");
    s.bind(
        "fz_y0",
        "greatest(arrayReduce('min', arrayFlatten(fz_rows)) - 1, 0)",
    );
    s.bind(
        "fz_y1",
        format!(
            "least(arrayReduce('max', arrayFlatten(fz_rows)) + 1, {})",
            VIEW_HEIGHT - 1
        ),
    );

    // The frame as it stood when the shadow started, over that corner only.
    // Everything the view put down covers every pixel of it, so sorting the
    // pixels by where they land gives the corner row by row.
    s.bind(
        "fz_under",
        "arrayFilter(t -> intDiv(t.1, 1048576) % 320 >= toUInt64(fz_x0) \
         AND intDiv(t.1, 1048576) % 320 <= toUInt64(fz_x1) \
         AND intDiv(intDiv(t.1, 1048576), 320) >= toUInt64(fz_y0) \
         AND intDiv(intDiv(t.1, 1048576), 320) <= toUInt64(fz_y1), \
         arrayConcat(wall_px, flat_px, sky_px, \
         arrayFilter(t -> t.1 % 1048576 < toUInt64(fz_time), arrayConcat(sp_px, ps_px))))",
    );
    s.bind("fz_sorted", "arraySort(t -> t.1, fz_under)");
    s.bind(
        "fz_sorted_at",
        "arrayMap(t -> toUInt32(intDiv(t.1, 1048576)), fz_sorted)",
    );
    s.bind(
        "fz_box",
        format!(
            "arrayMap(t -> t.2, arrayFilter((t, a, n) -> a != n, fz_sorted, fz_sorted_at, {}))",
            shifted("fz_sorted_at", "toUInt32(4294967295)", false)
        ),
    );
    // A corner the view left a hole in would read whatever fell into the gap.
    s.bind(
        "fz_box_guard",
        "throwIf(NOT empty(fz_cols) \
         AND length(fz_box) != toUInt64(fz_w) * (toUInt64(fz_y1) - toUInt64(fz_y0) + 1), \
         'the frame under a shadow has a hole in it')",
    );

    // One column at a time, carrying the row it last wrote and the colour it
    // put there, because the row above may be that one.
    let at = |row: &str| format!("fz_box[({row} - fz_y0) * fz_w + (c.2 - fz_x0) + 1]");
    let source = format!(
        "if(k_fuzzoffset[(prev_fuzzpos + start + t.2 - 1) % 50 + 1] > 0, {below}, \
         if(acc.1 = t.1 - 1, acc.2, {above}))",
        below = at("t.1 + 1"),
        above = at("t.1 - 1"),
    );
    let shade = pool("k_colormap", &format!("6 * 256 + {source}"));
    s.bind(
        "fz_px",
        format!(
            "arrayFlatten(arrayMap((c, rows, start) -> arrayFold((acc, t) -> \
             (t.1, {shade}, \
              arrayPushBack(acc.3, (toUInt64(t.1 * {VIEW_WIDTH} + c.2) * 1048576 \
                + toUInt64(c.1), {shade}))), \
             arrayZip(rows, arrayMap(e -> toUInt64(e), arrayEnumerate(rows))), \
             (toInt32(-2), toUInt8(0), CAST([], 'Array(Tuple(UInt64, UInt8))'))).3, \
             fz_cols, fz_rows, fz_start))"
        ),
    );
    // `fuzzpos` steps once per pixel drawn and carries into the next frame.
    s.bind(
        "fuzzpos",
        "toUInt8((toUInt64(prev_fuzzpos) + length(fz_px)) % 50)",
    );
}

// ---------------------------------------------------------------------------
// The framebuffer
// ---------------------------------------------------------------------------

/// The drawn pixels over the frame before, as bytes.
///
/// Everything drawn lands in one list of `(offset, colour)`. Sorted by
/// offset the drawn pixels fall into runs of consecutive offsets, so the
/// framebuffer is the previous frame's bytes with those runs cut in.
fn compose(s: &mut Stage) {
    s.bind(
        "prev_fb",
        format!(
            "coalesce(joinGetOrNull('{}.native_frames', 'fb', toUInt32(frame - 1)), \
             repeat('\\0', {FB_BYTES}))",
            s.db
        ),
    );
    s.bind(
        "prev_fb_bytes",
        format!(
            "joinGet('{}.native_frames', 'fb_bytes', toUInt32(frame - 1))",
            s.db
        ),
    );
    // Every drawn pixel, keyed by where it lands and when it was drawn. The
    // sort puts a pixel's writers in the order the engine ran them, and only
    // the last of each survives, which is what drawing over does.
    s.bind(
        "px_all",
        "arrayConcat(wall_px, flat_px, sky_px, sp_px, ps_px, hu_px, sb_px, fz_px)",
    );
    s.bind("px_ordered", "arraySort(t -> t.1, px_all)");
    s.bind(
        "px_ordered_at",
        "arrayMap(t -> toUInt32(intDiv(t.1, 1048576)), px_ordered)",
    );
    s.bind(
        "px_ordered_next",
        shifted("px_ordered_at", "toUInt32(4294967295)", false),
    );
    s.bind(
        "px_sorted",
        "arrayFilter((t, a, n) -> a != n, px_ordered, px_ordered_at, \
         px_ordered_next)",
    );
    s.bind(
        "px_at",
        "arrayMap(t -> toUInt32(intDiv(t.1, 1048576)), px_sorted)",
    );
    s.bind(
        "px_bytes",
        "arrayStringConcat(arrayMap(t -> char(t.2), px_sorted), '')",
    );
    s.bind("px_prev_at", shifted("px_at", "toUInt32(4294967295)", true));
    s.bind(
        "px_next_at",
        shifted("px_at", "toUInt32(4294967295)", false),
    );
    s.bind(
        "run_head",
        "arrayFilter((e, a, p) -> a != p + 1, arrayEnumerate(px_at), px_at, px_prev_at)",
    );
    s.bind(
        "run_start",
        "arrayFilter((a, p) -> a != p + 1, px_at, px_prev_at)",
    );
    s.bind(
        "run_end",
        "arrayFilter((a, n) -> n != a + 1, px_at, px_next_at)",
    );
    // Where the previous frame shows through: from just past the run before
    // to just before this one.
    s.bind(
        "run_gap_from",
        "if(empty(run_end), run_end, \
         arrayPushFront(arrayMap(e -> e + 1, arrayPopBack(run_end)), toUInt32(0)))",
    );
    s.bind(
        "fb",
        format!(
            "concat(arrayStringConcat(arrayMap((gf, rs, re, h) -> concat(\
             substring(prev_fb, gf + 1, rs - gf), substring(px_bytes, h, re - rs + 1)), \
             run_gap_from, run_start, run_end, run_head), ''), \
             substring(prev_fb, if(empty(run_end), 1, run_end[length(run_end)] + 2), \
             {FB_BYTES}))"
        ),
    );
    // The same bytes as an array, cut the same way. Going through the string
    // instead would read one byte at a time out of a 64,000-byte value, and
    // a lambda pays for a copy of that value per byte it reads.
    s.bind(
        "prev_bytes",
        format!(
            "if(empty(prev_fb_bytes), arrayMap(i -> toUInt8(0), range({FB_BYTES})), \
             prev_fb_bytes)"
        ),
    );
    s.bind("px_colours", "arrayMap(t -> t.2, px_sorted)");
    s.bind(
        "fb_bytes",
        format!(
            "arrayConcat(arrayFlatten(arrayMap((gf, rs, re, h) -> arrayConcat(\
             arraySlice(prev_bytes, gf + 1, rs - gf), arraySlice(px_colours, h, re - rs + 1)), \
             run_gap_from, run_start, run_end, run_head)), \
             arraySlice(prev_bytes, if(empty(run_end), 1, run_end[length(run_end)] + 2), \
             {FB_BYTES}))"
        ),
    );
    // `ST_doPaletteStuff`. Damage and berserk shift the screen red, a pickup
    // shifts it gold, and a radiation suit shifts it green. The numbering is
    // `PLAYPAL`'s own: 1 to 8 red, 9 to 12 gold, 13 green.
    s.state("st_damagecount", "p_damagecount");
    s.state("st_bonuscount", "p_bonuscount");
    s.bind(
        "pal_count",
        "greatest(st_damagecount, if(st_powers[2] != 0, 12 - bitShiftRight(st_powers[2], 6), \
         toInt32(0)))",
    );
    s.bind(
        "palette_index",
        "toUInt8(multiIf(\
         pal_count != 0, least(bitShiftRight(pal_count + 7, 3), 7) + 1, \
         st_bonuscount != 0, least(bitShiftRight(st_bonuscount + 7, 3), 3) + 9, \
         st_powers[4] > 128 OR bitAnd(st_powers[4], 8) != 0, 13, \
         0))",
    );
    s.bind("palette", "k_palettes[palette_index + 1]");
    s.bind(
        "rgb32",
        "arrayStringConcat(arrayMap(c -> substring(k_rgb[palette_index + 1], c * 4 + 1, 4), \
         fb_bytes), '')",
    );
    // The guard is read here so the frame cannot skip it.
    // The two guards are read here so the frame cannot skip them.
    s.bind(
        "fb_hash",
        "xxHash64(concat(fb, palette)) + hu_guard + sb_guard",
    );
    // What each widget drew, for the frame after this one to compare against.
    s.bind(
        "st_cache",
        "CAST((sb_ready, toInt32(0), st_health, st_armorpoints, st_ammo, st_maxammo, \
         arrayMap(i -> toInt32(st_weaponowned[i + 2]), range(6)), sb_keybox, st_face, \
         toInt32(1)), \
         'Tuple(ready Int32, frags Int32, health Int32, armor Int32, ammo Array(Int32), \
         maxammo Array(Int32), arms Array(Int32), keyboxes Array(Int32), faceindex Int32, \
         armsbg Int32)')",
    );
}

/// The row the frame writes. Every column is a stage of its own, so the
/// outer `SELECT` only names them.
fn output_columns() -> String {
    [
        "frame",
        "tic",
        "fb",
        "fb_bytes",
        "palette",
        "palette_index",
        "rgb32",
        "fb_hash",
        "fuzzpos",
        "st_cache",
    ]
    .map(|name| format!("    {name}"))
    .join(",\n")
}

/// An array shifted one place, with `fill` taking the place that opens up.
/// `arrayPushFront` on an empty array would make it one long, which every
/// caller here pairs with the array it came from.
fn shifted(arr: &str, fill: &str, forward: bool) -> String {
    let (drop, push) = if forward {
        ("arrayPopBack", "arrayPushFront")
    } else {
        ("arrayPopFront", "arrayPushBack")
    };
    format!("if(empty({arr}), {arr}, {push}({drop}({arr}), {fill}))")
}

/// A local name inside a lambda. ClickHouse has no `let`, and a one-element
/// `arrayMap` is the shortest thing that behaves like one.
fn let_in(bindings: &[(&str, String)], body: &str) -> String {
    let names = bindings
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(", ");
    let values = bindings
        .iter()
        .map(|(_, v)| format!("[{v}]"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("arrayMap(({names}) -> {body}, {values})[1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame 0 is the melt's first frame, so the padding row the server
    /// pre-reads has to be told apart by something other than the frame
    /// number.
    #[test]
    fn the_padding_row_is_told_apart_by_its_padding() {
        let sql = frame_transform("nat");
        assert!(sql.contains("WHERE empty(pad)"));
        assert!(!sql.contains("WHERE frame"));
    }

    #[test]
    fn the_statement_writes_the_frame_table_of_its_own_database() {
        assert!(frame_transform("nat").starts_with("INSERT INTO nat.native_frames"));
    }
}
