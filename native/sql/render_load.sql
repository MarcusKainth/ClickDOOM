-- The renderer's own tables, in SQL.
--
-- `{{DB}}` is the database and `{{SKY}}` the episode's sky texture. Every
-- statement reads the engine's constant tables or the decoded level and
-- writes one `rt_*` table. Together they are what `R_ExecuteSetViewSize`,
-- `R_InitTextureMapping`, `R_InitLightTables` and `R_InitSkyMap` compute for
-- `screenblocks = 10`: a 320x168 view at the top left of a 320x200 screen,
-- `centerx = 160`, `centery = 84`, `centerxfrac = projection = 10485760`,
-- `detailshift = 0`.
--
-- Those numbers are written out rather than named, because the view size is
-- fixed: the ROM never calls `R_SetViewSize` with anything else.
--
-- `FixedDiv` appears here as a plain `intDiv` of the widened numerator. Each
-- use states the divisor's own bound, and every one of them is far above the
-- `|a| >> 14` the C compares against, so the saturating arm is unreachable.

-- `viewangletox`. `x_raw` is the value the `xtoviewangle` scan reads, with
-- -1 and 321 still marking the two fenceposts; `x` is the same after the
-- pass that pulls them onto the edges of the view.
--
-- `focallength` is `FixedDiv(centerxfrac, finetangent[3072])`, and
-- `finetangent[3072]` is one FRACUNIT, so the divisor is far above 640.
INSERT INTO {{DB}}.rt_viewangletox (id, x_raw, x)
SELECT
    t.id,
    t.x_raw,
    multiIf(t.x_raw = -1, 0, t.x_raw = 321, 320, t.x_raw) AS x
FROM
(
    SELECT
        id,
        multiIf(
            value > 131072, -1,
            value < -131072, 321,
            raw < -1, -1,
            raw > 321, 321,
            raw) AS x_raw
    FROM
    (
        SELECT
            id,
            value,
            toInt32(bitShiftRight(
                toInt64(10485760)
                    - toInt64(toInt32(bitShiftRight(toInt64(value) * toInt64(f.len), 16)))
                    + 65535,
                16)) AS raw
        FROM {{DB}}.finetangent
        CROSS JOIN
        (
            SELECT toInt32(intDiv(bitShiftLeft(toInt64(10485760), 16), toInt64(value))) AS len
            FROM {{DB}}.finetangent WHERE id = 3072
        ) AS f
    )
) AS t;

-- `xtoviewangle[x]` is the first fine angle whose column is not past `x`,
-- turned back into an angle.
INSERT INTO {{DB}}.rt_xtoviewangle (id, angle)
SELECT
    x AS id,
    toUInt32(4294967296
             + bitShiftLeft(toInt64(arrayFirstIndex(t -> t <= x, v.tox) - 1), 19)
             - 1073741824) AS angle
FROM
(
    SELECT groupArray(x_raw) AS tox FROM (SELECT x_raw FROM {{DB}}.rt_viewangletox ORDER BY id)
) AS v
ARRAY JOIN range(321) AS x;

-- `yslope`, one per view row. The divisor is `|((y - 84) << 16) + 32768|`,
-- which is 32768 at its smallest.
INSERT INTO {{DB}}.rt_yslope (id, value)
SELECT
    toUInt32(number) AS id,
    toInt32(intDiv(bitShiftLeft(toInt64(10485760), 16),
                   abs(bitShiftLeft(toInt64(number) - 84, 16) + 32768))) AS value
FROM numbers(168);

-- `distscale`, one per column. `finecosine[k]` is `finesine[k + 2048]`, and
-- over the columns of a 90 degree field it never falls below 43000.
INSERT INTO {{DB}}.rt_distscale (id, value)
SELECT
    a.id,
    toInt32(intDiv(bitShiftLeft(toInt64(65536), 16),
                   abs(toInt64(s.m[bitShiftRight(a.angle, 19) + 2048])))) AS value
FROM {{DB}}.rt_xtoviewangle AS a
CROSS JOIN
(
    SELECT mapFromArrays(groupArray(id), groupArray(value)) AS m FROM {{DB}}.finesine
) AS s
WHERE a.id < 320;

-- `scalelight`: the colormap a wall of light level `light` takes at scale
-- index `scale`. `scale * SCREENWIDTH / viewwidth / DISTMAP` is `scale / 2`
-- at this view width.
INSERT INTO {{DB}}.rt_scalelight (light, scale, level)
SELECT
    toUInt8(number) AS light,
    toUInt8(j) AS scale,
    toUInt8(least(greatest(toInt64((15 - number) * 4) - intDiv(j, 2), 0), 31)) AS level
FROM numbers(16)
ARRAY JOIN range(48) AS j;

-- `zlight`: the colormap a flat takes at distance index `z`. The divisor
-- `(z + 1) << 20` is never below 1048576.
INSERT INTO {{DB}}.rt_zlight (light, z, level)
SELECT
    toUInt8(number) AS light,
    toUInt8(j) AS z,
    toUInt8(least(greatest(
        toInt64((15 - number) * 4)
            - intDiv(bitShiftRight(intDiv(bitShiftLeft(toInt64(10485760), 16),
                                          bitShiftLeft(toInt64(j) + 1, 20)), 12), 2),
        0), 31)) AS level
FROM numbers(16)
ARRAY JOIN range(128) AS j;

-- The sky. `skyflatnum` is the flat named `F_SKY1`, `skytexture` the texture
-- the episode names, and `skytexturemid` is 100 FRACUNITs.
INSERT INTO {{DB}}.rt_sky (id, flatnum, texture, texturemid)
SELECT
    0 AS id,
    (SELECT toInt32(id) FROM {{DB}}.flats WHERE upper(name) = 'F_SKY1') AS flatnum,
    (SELECT toInt32(id) FROM {{DB}}.tex_textures WHERE upper(name) = '{{SKY}}') AS texture,
    6553600 AS texturemid;

-- Which subsector owns each seg. `R_Subsector` takes the front sector from
-- the subsector, so a seg's own `frontsector` is not what the wall reads.
INSERT INTO {{DB}}.rt_seg_subsector (seg, subsector)
SELECT ss.firstline + k AS seg, ss.id AS subsector
FROM {{DB}}.lv_subsectors AS ss
ARRAY JOIN range(ss.numlines) AS k;

-- Every seg belongs to exactly one subsector, or a traversal would draw a
-- wall twice or not at all.
SELECT throwIf(
    (SELECT count() FROM {{DB}}.rt_seg_subsector) != (SELECT count() FROM {{DB}}.lv_segs),
    'a seg belongs to no subsector or to more than one');

-- A BSP path deeper than 63 would not fit the pre-order key the frame
-- transform sorts subsectors by.
SELECT throwIf(
    (SELECT max(depth) FROM {{DB}}.lv_ssec_path) > 63,
    'a subsector sits deeper than the pre-order key can hold');

-- A pool is one array per part, in byte order. `groupArray` does not promise
-- the order it collects rows in and `GROUP BY` collects them in parallel, so
-- each byte carries the offset it belongs at and the array is sorted by it.
--
-- A pool is one string, in byte order. `groupArray` does not promise the
-- order it collects rows in, so every byte carries the offset it belongs at
-- and is sorted by that before the bytes are joined.
--
-- A pool is a string and not an array. Both work, but a query holds an array
-- constant as one field per element, and carrying a few hundred thousand of
-- those through the frame statement costs gigabytes; a string is one field
-- whatever its length.

-- The texture window pool: every column's 128 bytes, end to end in slot
-- order.
INSERT INTO {{DB}}.rt_tex_pool (id, data)
SELECT 0 AS id, arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
FROM
(
    SELECT
        slot * 128 + k AS at,
        reinterpretAsUInt8(substring(rightPad(window, 128, '\0'), k + 1, 1)) AS b
    FROM {{DB}}.tex_window
    ARRAY JOIN range(128) AS k
);

-- Every column of every texture has to be in it, or a wall reads the bytes
-- of the column after the one it asked for.
SELECT throwIf(
    (SELECT length(data) FROM {{DB}}.rt_tex_pool) !=
        (SELECT count() FROM {{DB}}.tex_window) * 128,
    'the texture pool is not one window per column');

-- The flat pool: every flat's 4,096 bytes, end to end in flat number order.
-- A marker lump carries no pixels and its 4,096 bytes are zero, which keeps
-- a flat's pixels at `id * 4096`.
INSERT INTO {{DB}}.rt_flat_pool (id, data)
SELECT 0 AS id, arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
FROM
(
    SELECT
        f * 4096 + k AS at,
        reinterpretAsUInt8(substring(rightPad(coalesce(fl.data, ''), 4096, '\0'), k + 1, 1)) AS b
    FROM numbers(512)
    ARRAY JOIN [toUInt32(number)] AS f
    LEFT JOIN {{DB}}.flats AS fl ON fl.id = f
    ARRAY JOIN range(4096) AS k
);

-- The pool is numbered by flat, so a flat past the 512 it holds would read
-- another flat's pixels.
SELECT throwIf(
    (SELECT max(id) FROM {{DB}}.flats) >= 512,
    'the flat pool holds fewer flats than the WAD does');

INSERT INTO {{DB}}.rt_colormap_pool (id, data)
SELECT 0 AS id, arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
FROM
(
    SELECT
        id * 256 + k AS at,
        reinterpretAsUInt8(substring(data, k + 1, 1)) AS b
    FROM {{DB}}.colormap
    ARRAY JOIN range(256) AS k
);

-- The palettes the ROM writes: `gammatable[0][PLAYPAL[i]]`, and the same
-- bytes again as the 0RGB words a display takes.
INSERT INTO {{DB}}.rt_palette (id, data, rgb)
SELECT
    p.id,
    arrayStringConcat(arrayMap(k -> char(g.m[reinterpretAsUInt8(substring(p.data, k + 1, 1))]),
                               range(768)), '') AS data,
    arrayStringConcat(arrayMap(k -> concat('\0',
        char(g.m[reinterpretAsUInt8(substring(p.data, k * 3 + 1, 1))]),
        char(g.m[reinterpretAsUInt8(substring(p.data, k * 3 + 2, 1))]),
        char(g.m[reinterpretAsUInt8(substring(p.data, k * 3 + 3, 1))])), range(256)), '') AS rgb
FROM {{DB}}.playpal AS p
CROSS JOIN
(
    SELECT mapFromArrays(groupArray(id), groupArray(value)) AS m
    FROM {{DB}}.gammatable WHERE level = 0
) AS g;

-- ---------------------------------------------------------------------------
-- Sprites
-- ---------------------------------------------------------------------------

-- Where each sprite lump's bytes start in the pool, and the fields
-- `R_ProjectSprite` reads off it.
INSERT INTO {{DB}}.rt_sprite_lump (id, base, width, width_fixed, leftoffset, topoffset)
SELECT
    sl.id,
    toUInt32(sum(length(l.bytes)) OVER (ORDER BY sl.id
        ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)) AS base,
    sl.width,
    sl.width_fixed,
    sl.leftoffset,
    sl.topoffset
FROM {{DB}}.sprite_lumps AS sl
INNER JOIN {{DB}}.wad_lumps AS l ON l.id = sl.lump;

-- The pool itself: every lump's bytes, in sprite number order.
INSERT INTO {{DB}}.rt_sprite_pool (id, data)
SELECT 0 AS id, arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
FROM
(
    SELECT
        sb.base + k AS at,
        reinterpretAsUInt8(substring(l.bytes, k + 1, 1)) AS b
    FROM {{DB}}.rt_sprite_lump AS sb
    INNER JOIN {{DB}}.sprite_lumps AS sl ON sl.id = sb.id
    INNER JOIN {{DB}}.wad_lumps AS l ON l.id = sl.lump
    ARRAY JOIN range(toUInt32(length(l.bytes))) AS k
);

-- Every lump has to be in it, or a sprite reads the lump after the one it
-- asked for.
SELECT throwIf(
    (SELECT length(data) FROM {{DB}}.rt_sprite_pool) !=
        (SELECT sum(length(l.bytes)) FROM {{DB}}.sprite_lumps AS sl
         INNER JOIN {{DB}}.wad_lumps AS l ON l.id = sl.lump),
    'the sprite pool is not every sprite lump end to end');

-- `spriteframe_t`, one row per rotation. A frame that serves every rotation
-- with one picture repeats it eight times, which is what `sprframe->lump[0]`
-- reads when `rotate` is zero.
-- Every slot carries a row, so the frame transform indexes rather than
-- searches. A slot no frame reaches carries lump -1.
INSERT INTO {{DB}}.rt_sprite_frame (slot, rotate, lump, flip)
SELECT slot, toUInt8(0) AS rotate, toInt32(-1) AS lump, toUInt8(0) AS flip
FROM numbers(32768)
ARRAY JOIN [toUInt32(number)] AS slot
WHERE slot NOT IN
(
    SELECT (sprite * 32 + frame) * 8 + r
    FROM {{DB}}.sprite_frames ARRAY JOIN range(8) AS r
)
UNION ALL
SELECT
    (sprite * 32 + frame) * 8 + r AS slot,
    rotate,
    lump[r + 1] AS lump,
    flip[r + 1] AS flip
FROM {{DB}}.sprite_frames
ARRAY JOIN range(8) AS r;

-- A frame past 32, or a sprite past the table, would land on another
-- frame's slot.
SELECT throwIf(
    (SELECT max(frame) FROM {{DB}}.sprite_frames) >= 32,
    'a sprite has more frames than the frame slot can hold');

-- Every sprite post, in column order, and where each column's run of them
-- starts. `patch_posts` is what the engine's own column walk reads, so a
-- draw that runs past a post reads the bytes the engine reads. Both
-- statements number the posts by the same window function over the same
-- order, so the two agree whatever the server splits the read into.
INSERT INTO {{DB}}.rt_sprite_post (id, topdelta, length, ofs)
SELECT
    toUInt32(row_number() OVER (ORDER BY lump, col, idx) - 1) AS id,
    topdelta,
    length,
    base + ofs AS ofs
FROM
(
    SELECT sl.id AS lump, pp.col, pp.idx, pp.topdelta, pp.length, pp.ofs, sb.base
    FROM {{DB}}.patch_posts AS pp
    INNER JOIN {{DB}}.sprite_lumps AS sl ON sl.lump = pp.lump
    INNER JOIN {{DB}}.rt_sprite_lump AS sb ON sb.id = sl.id
);

-- Every slot carries a row here too. A column with no posts carries none.
INSERT INTO {{DB}}.rt_sprite_colposts (slot, first, num)
SELECT slot, toUInt32(0) AS first, toUInt16(0) AS num
FROM numbers(131072)
ARRAY JOIN [toUInt32(number)] AS slot
WHERE slot NOT IN
(
    SELECT sl.id * 256 + pp.col
    FROM {{DB}}.patch_posts AS pp
    INNER JOIN {{DB}}.sprite_lumps AS sl ON sl.lump = pp.lump
)
UNION ALL
SELECT
    lump * 256 + col AS slot,
    toUInt32(min(id)) AS first,
    toUInt16(count()) AS num
FROM
(
    SELECT
        row_number() OVER (ORDER BY lump, col, idx) - 1 AS id,
        lump,
        col
    FROM
    (
        SELECT sl.id AS lump, pp.col, pp.idx
        FROM {{DB}}.patch_posts AS pp
        INNER JOIN {{DB}}.sprite_lumps AS sl ON sl.lump = pp.lump
    )
)
GROUP BY slot, lump, col;

-- A sprite wider than 256 columns would land on the next lump's slots.
SELECT throwIf(
    (SELECT max(width) FROM {{DB}}.sprite_lumps) > 256,
    'a sprite is wider than the column slot can hold');

-- A two-sided line with a middle texture draws that texture over the gap,
-- post by post. The renderer takes such a line's silhouette into account but
-- does not draw its pixels, so a map that has one fails here rather than
-- rendering a frame with a hole in it.
SELECT throwIf(
    (SELECT count() FROM {{DB}}.tex_posts) > 0,
    'a line draws a masked middle texture, which the renderer does not draw');

-- ---------------------------------------------------------------------------
-- The heads-up message
-- ---------------------------------------------------------------------------

-- Each message under the hash the state row names it by.
INSERT INTO {{DB}}.rt_message (hash, name, text)
SELECT xxHash64(text) AS hash, name, text FROM {{DB}}.messages;

-- Two messages under one hash would draw the wrong one.
SELECT throwIf(
    (SELECT count() FROM {{DB}}.rt_message) !=
    (SELECT uniqExact(hash) FROM {{DB}}.rt_message),
    'two messages hash to the same value');

-- ---------------------------------------------------------------------------
-- The status bar and heads-up graphics
-- ---------------------------------------------------------------------------

-- Every patch, numbered by name, with where its bytes start in the pool.
INSERT INTO {{DB}}.rt_ui_patch (id, name, base, width, height, leftoffset, topoffset)
SELECT
    toUInt32(row_number() OVER (ORDER BY name) - 1) AS id,
    name,
    toUInt32(sum(length(data)) OVER (ORDER BY name
        ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)) AS base,
    width,
    height,
    leftoffset,
    topoffset
FROM {{DB}}.ui_patches;

-- The pool: every patch's bytes, in the order `rt_ui_patch` numbers them.
INSERT INTO {{DB}}.rt_ui_pool (id, data)
SELECT 0 AS id, arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
FROM
(
    SELECT
        up.base + k AS at,
        reinterpretAsUInt8(substring(u.data, k + 1, 1)) AS b
    FROM {{DB}}.rt_ui_patch AS up
    INNER JOIN {{DB}}.ui_patches AS u ON u.name = up.name
    ARRAY JOIN range(toUInt32(length(u.data))) AS k
);

SELECT throwIf(
    (SELECT length(data) FROM {{DB}}.rt_ui_pool) !=
        (SELECT sum(length(data)) FROM {{DB}}.ui_patches),
    'the heads-up pool is not every patch end to end');

-- The posts of every patch column, and where each column's run starts.
INSERT INTO {{DB}}.rt_ui_post (id, topdelta, length, ofs)
SELECT
    toUInt32(row_number() OVER (ORDER BY patch, col, idx) - 1) AS id,
    topdelta,
    length,
    base + ofs AS ofs
FROM
(
    SELECT up.id AS patch, pp.col, pp.idx, pp.topdelta, pp.length, pp.ofs, up.base
    FROM {{DB}}.patch_posts AS pp
    INNER JOIN {{DB}}.ui_patches AS u ON u.id = pp.lump
    INNER JOIN {{DB}}.rt_ui_patch AS up ON up.name = u.name
);

INSERT INTO {{DB}}.rt_ui_colposts (slot, first, num)
SELECT slot, toUInt32(0) AS first, toUInt16(0) AS num
FROM numbers(131072)
ARRAY JOIN [toUInt32(number)] AS slot
WHERE slot NOT IN
(
    SELECT up.id * 512 + pp.col
    FROM {{DB}}.patch_posts AS pp
    INNER JOIN {{DB}}.ui_patches AS u ON u.id = pp.lump
    INNER JOIN {{DB}}.rt_ui_patch AS up ON up.name = u.name
)
UNION ALL
SELECT
    patch * 512 + col AS slot,
    toUInt32(min(id)) AS first,
    toUInt16(count()) AS num
FROM
(
    SELECT row_number() OVER (ORDER BY patch, col, idx) - 1 AS id, patch, col
    FROM
    (
        SELECT up.id AS patch, pp.col, pp.idx
        FROM {{DB}}.patch_posts AS pp
        INNER JOIN {{DB}}.ui_patches AS u ON u.id = pp.lump
        INNER JOIN {{DB}}.rt_ui_patch AS up ON up.name = u.name
    )
)
GROUP BY slot, patch, col;

SELECT throwIf(
    (SELECT max(id) FROM {{DB}}.rt_ui_patch) >= 256
        OR (SELECT max(width) FROM {{DB}}.ui_patches) > 512,
    'a heads-up patch is past what the column slot can hold');

-- What each thing the engine draws is, in one numbering the frame transform
-- indexes. `ST_loadUnloadGraphics` and `HU_Init` name the same lumps.
--
--     0 to 9      STTNUM0..9,  the tall digits
--    10 to 19     STYSNUM0..9, the short digits
--    20           STTPRCNT,    the tall percent sign
--    21 to 26     STKEYS0..5,  the key cards
--    27           STARMS,      the arms background
--    28 to 33     STGNUM2..7,  the grey weapon numbers
--    34 to 75     the face, in `ST_loadUnloadGraphics`'s own order
--    76           STBAR,       the status bar itself
--    77 to 139    STCFN033..095, the heads-up font
--   140           STTMINUS,    the minus sign
INSERT INTO {{DB}}.rt_ui_slot (slot, name, patch)
SELECT s.slot, s.name, p.id AS patch
FROM
(
    SELECT toUInt32(i) AS slot, concat('STTNUM', toString(i)) AS name
    FROM numbers(10) ARRAY JOIN [toUInt32(number)] AS i
    UNION ALL
    SELECT toUInt32(10 + i), concat('STYSNUM', toString(i))
    FROM numbers(10) ARRAY JOIN [toUInt32(number)] AS i
    UNION ALL
    SELECT toUInt32(20), 'STTPRCNT'
    UNION ALL
    SELECT toUInt32(21 + i), concat('STKEYS', toString(i))
    FROM numbers(6) ARRAY JOIN [toUInt32(number)] AS i
    UNION ALL
    SELECT toUInt32(27), 'STARMS'
    UNION ALL
    SELECT toUInt32(28 + i), concat('STGNUM', toString(i + 2))
    FROM numbers(6) ARRAY JOIN [toUInt32(number)] AS i
    UNION ALL
    SELECT
        toUInt32(34 + k),
        multiIf(
            k % 8 < 3, concat('STFST', toString(intDiv(k, 8)), toString(k % 8)),
            k % 8 = 3, concat('STFTR', toString(intDiv(k, 8)), '0'),
            k % 8 = 4, concat('STFTL', toString(intDiv(k, 8)), '0'),
            k % 8 = 5, concat('STFOUCH', toString(intDiv(k, 8))),
            k % 8 = 6, concat('STFEVL', toString(intDiv(k, 8))),
            concat('STFKILL', toString(intDiv(k, 8))))
    FROM numbers(40) ARRAY JOIN [toUInt32(number)] AS k
    UNION ALL
    SELECT toUInt32(74), 'STFGOD0'
    UNION ALL
    SELECT toUInt32(75), 'STFDEAD0'
    UNION ALL
    SELECT toUInt32(76), 'STBAR'
    UNION ALL
    SELECT toUInt32(77 + i), concat('STCFN', leftPad(toString(i + 33), 3, '0'))
    FROM numbers(63) ARRAY JOIN [toUInt32(number)] AS i
    UNION ALL
    SELECT toUInt32(140), 'STTMINUS'
) AS s
INNER JOIN {{DB}}.rt_ui_patch AS p ON p.name = s.name;

-- A slot that found no patch would draw nothing where the engine drew
-- something.
SELECT throwIf(
    (SELECT count() FROM {{DB}}.rt_ui_slot) != 141,
    'a status bar or font patch is missing from the WAD');

-- The status bar with nothing on it. `ST_refreshBackground` draws `STBAR` at
-- the top left of its own buffer, and every widget copies its own area back
-- out of that buffer before it draws.
INSERT INTO {{DB}}.rt_ui_backing (id, data)
SELECT 0 AS id, arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
FROM
(
    SELECT
        (toUInt32(pp.topdelta) + k) * 320 + pp.col AS at,
        reinterpretAsUInt8(substring(u.data, pp.ofs + k + 1, 1)) AS b
    FROM {{DB}}.patch_posts AS pp
    INNER JOIN {{DB}}.ui_patches AS u ON u.id = pp.lump
    ARRAY JOIN range(toUInt32(pp.length)) AS k
    WHERE u.name = 'STBAR'
);

-- `STBAR` covers all 320 by 32 of it. A pixel it leaves out would read
-- whatever the engine's own buffer held there.
SELECT throwIf(
    (SELECT length(data) FROM {{DB}}.rt_ui_backing) != 10240,
    'STBAR does not cover the whole status bar');
