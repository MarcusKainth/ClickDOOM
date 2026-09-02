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
SELECT
    0 AS id,
    arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
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
SELECT
    0 AS id,
    arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
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
SELECT
    0 AS id,
    arrayStringConcat(arrayMap(t -> char(t.2), arraySort(t -> t.1, groupArray((at, b)))), '') AS data
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
