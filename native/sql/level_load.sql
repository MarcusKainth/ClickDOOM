-- Decoding one map and its assets, in SQL.
--
-- `{{DB}}` is the database, `{{MAP}}` the map marker and `{{DEMO}}` the
-- demo lump. Every statement reads `wad_lumps`, which holds the WAD's lumps
-- as raw bytes, and writes a derived table. Nothing outside ClickHouse
-- decodes a record, resolves a name to a number or composes a texture.
--
-- Order matters. Textures and flats come first, because a sidedef and a
-- sector name their pictures and the map tables hold the numbers. The
-- derived geometry comes last, because it reads what the map decode wrote.
--
-- Reading a record: a lump of fixed-width records is `ARRAY JOIN`ed over
-- `range(length(bytes) / size)` and each field is a `substring` at its
-- offset. A `short` scaled to `fixed_t` is written as
-- `reinterpretAsInt32(concat('\0\0', <the two bytes>))`, which is the C
-- `short << FRACBITS` including its wrap, rather than a multiply that would
-- widen. A name is `splitByChar('\0', <the eight bytes>)[1]`, which is the
-- run before the terminator.
--
-- A name that resolves to nothing would read as picture 0 and draw the
-- wrong thing, so each resolution is followed by a statement that throws
-- when any name failed.

-- ---------------------------------------------------------------------------
-- Patch names and textures
-- ---------------------------------------------------------------------------

-- PNAMES: a count, then 8-byte names. `lump` is the highest-numbered lump
-- carrying the name, which is what `W_CheckNumForName` resolves to, and -1
-- for a name no lump carries. PNAMES holds names for patches this WAD does
-- not ship; only a texture that names one is an error.
--
-- Names are upper-cased on both sides of the lookup, because
-- `W_CheckNumForName` compares with `strncasecmp` and PNAMES spells one
-- name in lower case.
INSERT INTO {{DB}}.pnames (id, name, lump)
SELECT
    i AS id,
    upper(splitByChar('\0', substring(p.bytes, 4 + i * 8 + 1, 8))[1]) AS name,
    toInt32(if(has(w.m, name), w.m[name], -1)) AS lump
FROM {{DB}}.wad_lumps AS p
CROSS JOIN
(
    SELECT mapFromArrays(groupArray(name), groupArray(id)) AS m
    FROM (SELECT upper(name) AS name, max(id) AS id FROM {{DB}}.wad_lumps GROUP BY name)
) AS w
ARRAY JOIN range(reinterpretAsUInt32(substring(p.bytes, 1, 4))) AS i
WHERE p.name = 'PNAMES';

-- TEXTURE1: a count, then one offset per texture, then a `maptexture_t` at
-- each. `widthmask` is `R_InitTextures`'s `texturewidthmask`: the largest
-- power of two not past the width, minus one.
INSERT INTO {{DB}}.tex_textures
    (id, name, width, height, widthmask, height_fixed, masked, patchcount)
SELECT
    i AS id,
    upper(splitByChar('\0', substring(t.bytes, o + 1, 8))[1]) AS name,
    reinterpretAsUInt16(substring(t.bytes, o + 13, 2)) AS width,
    reinterpretAsUInt16(substring(t.bytes, o + 15, 2)) AS height,
    toUInt16(bitShiftLeft(toUInt32(1), toUInt32(floor(log2(width)))) - 1) AS widthmask,
    reinterpretAsInt32(concat('\0\0', substring(t.bytes, o + 15, 2))) AS height_fixed,
    toUInt8(reinterpretAsInt32(substring(t.bytes, o + 9, 4)) != 0) AS masked,
    reinterpretAsUInt16(substring(t.bytes, o + 21, 2)) AS patchcount
FROM
(
    SELECT
        l.bytes AS bytes,
        i,
        reinterpretAsInt32(substring(l.bytes, 4 + i * 4 + 1, 4)) AS o
    FROM {{DB}}.wad_lumps AS l
    ARRAY JOIN range(reinterpretAsInt32(substring(l.bytes, 1, 4))) AS i
    WHERE l.name = 'TEXTURE1'
) AS t;

-- The `mappatch_t` entries after each `maptexture_t`.
INSERT INTO {{DB}}.tex_patches (texture, idx, originx, originy, patch, lump)
SELECT x.texture, x.idx, x.originx, x.originy, x.patch, p.lump
FROM
(
    SELECT
        t.i AS texture,
        j AS idx,
        reinterpretAsInt16(substring(t.bytes, t.o + 23 + j * 10, 2)) AS originx,
        reinterpretAsInt16(substring(t.bytes, t.o + 25 + j * 10, 2)) AS originy,
        toUInt32(reinterpretAsUInt16(substring(t.bytes, t.o + 27 + j * 10, 2))) AS patch
    FROM
    (
        SELECT
            l.bytes AS bytes,
            i,
            reinterpretAsInt32(substring(l.bytes, 4 + i * 4 + 1, 4)) AS o
        FROM {{DB}}.wad_lumps AS l
        ARRAY JOIN range(reinterpretAsInt32(substring(l.bytes, 1, 4))) AS i
        WHERE l.name = 'TEXTURE1'
    ) AS t
    ARRAY JOIN range(reinterpretAsUInt16(substring(t.bytes, t.o + 21, 2))) AS j
) AS x
INNER JOIN {{DB}}.pnames AS p ON p.id = x.patch;

-- A patch name with no lump behind it would compose as nothing.
SELECT throwIf(count() > 0, 'a texture names a patch the WAD does not hold')
FROM {{DB}}.tex_patches WHERE lump < 0;

-- ---------------------------------------------------------------------------
-- Patches
-- ---------------------------------------------------------------------------

-- Every lump in `patch_t` form: the patches TEXTURE1 names, the sprites
-- between the `S_` markers, and the status bar and font graphics.
INSERT INTO {{DB}}.patch_lumps (lump, name, width, height, leftoffset, topoffset, columnofs)
SELECT
    l.id AS lump,
    l.name,
    reinterpretAsUInt16(substring(l.bytes, 1, 2)) AS width,
    reinterpretAsUInt16(substring(l.bytes, 3, 2)) AS height,
    reinterpretAsInt16(substring(l.bytes, 5, 2)) AS leftoffset,
    reinterpretAsInt16(substring(l.bytes, 7, 2)) AS topoffset,
    arrayMap(c -> reinterpretAsUInt32(substring(l.bytes, 9 + c * 4, 4)), range(width)) AS columnofs
FROM {{DB}}.wad_lumps AS l
WHERE length(l.bytes) > 8 AND l.id IN
(
    SELECT lump FROM {{DB}}.pnames WHERE lump >= 0
    UNION DISTINCT
    SELECT id FROM {{DB}}.wad_lumps
    WHERE id > (SELECT id FROM {{DB}}.wad_lumps WHERE name = 'S_START')
      AND id < (SELECT id FROM {{DB}}.wad_lumps WHERE name = 'S_END')
    UNION DISTINCT
    SELECT id FROM {{DB}}.wad_lumps
    WHERE name IN ('STBAR', 'STARMS', 'STTMINUS', 'STTPRCNT')
       OR name LIKE 'STTNUM%' OR name LIKE 'STYSNUM%' OR name LIKE 'STGNUM%'
       OR name LIKE 'STKEYS%' OR name LIKE 'STF%' OR name LIKE 'STCFN%'
);

-- Each column's posts. A post is a topdelta, a length, a pad byte, that
-- many pixels and a second pad; the chain ends at a topdelta of 255. The
-- fold walks at most 64 posts, which is more than a column of a byte-tall
-- patch can hold, and the guard below is what says every walk reached the
-- terminator.
INSERT INTO {{DB}}.patch_posts (lump, col, idx, topdelta, length, ofs)
SELECT
    w.lump,
    w.col,
    toUInt16(n - 1) AS idx,
    post.1 AS topdelta,
    post.2 AS length,
    post.3 AS ofs
FROM
(
    SELECT
        p.lump AS lump,
        toUInt16(c) AS col,
        arrayFold(
            (acc, k) -> if(
                acc.1 >= toUInt32(length(l.bytes))
                    OR reinterpretAsUInt8(substring(l.bytes, acc.1 + 1, 1)) = 255,
                acc,
                (toUInt32(acc.1 + reinterpretAsUInt8(substring(l.bytes, acc.1 + 2, 1)) + 4),
                 arrayPushBack(acc.2, (reinterpretAsUInt8(substring(l.bytes, acc.1 + 1, 1)),
                                       reinterpretAsUInt8(substring(l.bytes, acc.1 + 2, 1)),
                                       toUInt32(acc.1 + 3))))),
            range(64),
            (p.columnofs[c + 1], CAST([], 'Array(Tuple(UInt8, UInt8, UInt32))'))).2 AS posts
    FROM {{DB}}.patch_lumps AS p
    INNER JOIN {{DB}}.wad_lumps AS l ON l.id = p.lump
    ARRAY JOIN range(p.width) AS c
) AS w
ARRAY JOIN w.posts AS post, arrayEnumerate(w.posts) AS n
SETTINGS max_block_size = 256;

-- A column whose chain did not reach a 255 was cut short by the fold's own
-- bound, and its later posts are missing.
SELECT throwIf(count() > 0, 'a patch column has more posts than the walk reads')
FROM
(
    SELECT p.lump, c AS col, max(q.ofs + q.length + 1) AS ends
    FROM {{DB}}.patch_lumps AS p
    ARRAY JOIN range(p.width) AS c
    INNER JOIN {{DB}}.patch_posts AS q ON q.lump = p.lump AND q.col = c
    GROUP BY p.lump, c
) AS e
INNER JOIN {{DB}}.wad_lumps AS l ON l.id = e.lump
WHERE reinterpretAsUInt8(substring(l.bytes, e.ends + 1, 1)) != 255;

-- ---------------------------------------------------------------------------
-- Texture composition
-- ---------------------------------------------------------------------------

-- Which patches land on each texture column. A patch covers the columns
-- from its `originx`, clipped to the texture's own width, which is what
-- `R_GenerateLookup` counts and `R_GenerateComposite` walks.
INSERT INTO {{DB}}.tex_col_patches (texture, col, idx, lump, patch_col, originy)
SELECT
    tp.texture,
    toUInt16(tp.originx + k) AS col,
    tp.idx,
    tp.lump,
    toUInt16(k) AS patch_col,
    tp.originy
FROM {{DB}}.tex_patches AS tp
INNER JOIN {{DB}}.patch_lumps AS pl ON pl.lump = toUInt32(tp.lump)
INNER JOIN {{DB}}.tex_textures AS tx ON tx.id = tp.texture
ARRAY JOIN range(pl.width) AS k
WHERE tp.originx + k >= 0 AND tp.originx + k < tx.width;

-- `R_GenerateLookup` errors on a column no patch reaches.
SELECT throwIf(count() > 0, 'a texture has a column no patch covers')
FROM
(
    SELECT tx.id AS texture, c AS col
    FROM {{DB}}.tex_textures AS tx
    ARRAY JOIN range(tx.width) AS c
) AS want
LEFT ANTI JOIN {{DB}}.tex_col_patches AS have
    ON have.texture = want.texture AND have.col = toUInt16(want.col);

-- `R_GenerateLookup`'s per-column result. A column one patch covers reads
-- straight out of that patch's lump, at the first post's pixels. A column
-- more than one covers reads out of the texture's composite, which grows
-- by the texture's height for each such column in column order.
--
-- `uncovered` walks the same clipped writes `R_GenerateComposite` makes
-- and says whether they reach every row of a composed column. A column one
-- patch covers is read out of that patch and never out of the composite,
-- so it is never flagged.
INSERT INTO {{DB}}.tex_columns (texture, col, patches, lump, ofs, uncovered)
SELECT
    g.texture AS texture,
    g.col AS col,
    g.patches AS patches,
    if(g.patches = 1, toInt32(g.one_lump), -1) AS lump,
    if(g.patches = 1,
       g.one_ofs,
       toUInt32(sum(if(g.patches = 1, 0, g.height))
                OVER (PARTITION BY g.texture ORDER BY g.col
                      ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING))) AS ofs,
    g.uncovered AS uncovered
FROM
(
    SELECT
        cnt.texture AS texture,
        cnt.col AS col,
        cnt.height AS height,
        cnt.patches AS patches,
        cnt.one_lump AS one_lump,
        cnt.one_ofs AS one_ofs,
        toUInt8(cnt.patches > 1 AND position(cov.mask, '\0') > 0) AS uncovered
    FROM
    (
        -- `R_GenerateLookup` counts patches by their column range, not by
        -- what they draw, so a patch column holding no post still counts.
        SELECT
            cp.texture AS texture,
            cp.col AS col,
            tx.height AS height,
            toUInt16(count()) AS patches,
            argMin(cp.lump, cp.idx) AS one_lump,
            argMin(pl.columnofs[cp.patch_col + 1] + 3, cp.idx) AS one_ofs
        FROM {{DB}}.tex_col_patches AS cp
        INNER JOIN {{DB}}.tex_textures AS tx ON tx.id = cp.texture
        INNER JOIN {{DB}}.patch_lumps AS pl ON pl.lump = cp.lump
        GROUP BY cp.texture, cp.col, tx.height
    ) AS cnt
    LEFT JOIN
    (
        -- The same clipped writes `R_GenerateComposite` makes, as a mask.
        -- A post that starts above the texture is shortened and moved to
        -- row 0, and one that runs past the bottom is cut there.
        SELECT
            cp.texture AS texture,
            cp.col AS col,
            arrayFold(
                (acc, w) -> concat(substring(acc, 1, w.1), repeat('\x01', w.2),
                                   substring(acc, w.1 + w.2 + 1)),
                arrayMap(w -> (w.2, w.3),
                         arraySort(w -> w.1, groupArray((toUInt32(cp.idx) * 256 + q.idx,
                                                         cover.1, cover.2)))),
                repeat('\0', tx.height)) AS mask
        FROM {{DB}}.tex_col_patches AS cp
        INNER JOIN {{DB}}.tex_textures AS tx ON tx.id = cp.texture
        INNER JOIN {{DB}}.patch_posts AS q ON q.lump = cp.lump AND q.col = cp.patch_col
        ARRAY JOIN [(toUInt32(greatest(cp.originy + q.topdelta, 0)),
                     toUInt32(greatest(least(toInt32(q.length) + least(cp.originy + q.topdelta, 0),
                                             toInt32(tx.height) - greatest(cp.originy + q.topdelta, 0)),
                                       0)))] AS cover
        GROUP BY cp.texture, cp.col, tx.height
    ) AS cov ON cov.texture = cnt.texture AND cov.col = cnt.col
) AS g;

-- `R_GenerateComposite`'s buffer: the composed columns end to end, in
-- column order, each the texture's height. A row no post reaches keeps the
-- zero the buffer starts at.
INSERT INTO {{DB}}.tex_composite (texture, data)
SELECT
    texture,
    arrayStringConcat(arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((col, data)))), '') AS data
FROM
(
    SELECT
        cp.texture AS texture,
        cp.col AS col,
        arrayFold(
            (acc, w) -> concat(substring(acc, 1, w.1), w.2, substring(acc, w.1 + length(w.2) + 1)),
            arrayMap(w -> (w.2, w.3),
                     arraySort(w -> w.1, groupArray((toUInt32(cp.idx) * 256 + q.idx,
                                                     write.1, write.2)))),
            repeat('\0', tx.height)) AS data
    FROM {{DB}}.tex_col_patches AS cp
    INNER JOIN {{DB}}.tex_textures AS tx ON tx.id = cp.texture
    INNER JOIN {{DB}}.tex_columns AS tc ON tc.texture = cp.texture AND tc.col = cp.col
    INNER JOIN {{DB}}.patch_posts AS q ON q.lump = cp.lump AND q.col = cp.patch_col
    INNER JOIN {{DB}}.wad_lumps AS l ON l.id = cp.lump
    ARRAY JOIN [(toUInt32(greatest(cp.originy + q.topdelta, 0)),
                 substring(l.bytes, q.ofs + 1,
                     toUInt32(greatest(least(toInt32(q.length) + least(cp.originy + q.topdelta, 0),
                                             toInt32(tx.height) - greatest(cp.originy + q.topdelta, 0)),
                                       0))))] AS write
    WHERE tc.patches > 1
    GROUP BY cp.texture, cp.col, tx.height
)
GROUP BY texture;

-- A composite has to be as long as the offsets the lookup handed out.
SELECT throwIf(count() > 0, 'a composite is shorter than the offsets into it')
FROM {{DB}}.tex_columns AS tc
INNER JOIN {{DB}}.tex_composite AS tk ON tk.texture = tc.texture
INNER JOIN {{DB}}.tex_textures AS tx ON tx.id = tc.texture
WHERE tc.lump < 0 AND tc.ofs + tx.height > length(tk.data);

-- The first window slot of each texture, so a column's window sits at
-- `base + col`.
INSERT INTO {{DB}}.tex_col_base (texture, base)
SELECT
    id AS texture,
    toUInt32(sum(width) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)) AS base
FROM {{DB}}.tex_textures;

-- One window per texture column: the 128 bytes a draw reads, zero padded.
-- `overrun` is 1 when the source held fewer than 128 bytes from the
-- column's offset, so the padding is what a draw past the end reads.
INSERT INTO {{DB}}.tex_window (slot, texture, col, window, overrun)
SELECT
    b.base + tc.col AS slot,
    tc.texture,
    tc.col,
    rightPad(src, 128, '\0') AS window,
    toUInt8(length(src) < 128) AS overrun
FROM {{DB}}.tex_columns AS tc
INNER JOIN {{DB}}.tex_col_base AS b ON b.texture = tc.texture
LEFT JOIN {{DB}}.wad_lumps AS l ON l.id = toUInt32(greatest(tc.lump, 0))
LEFT JOIN {{DB}}.tex_composite AS tk ON tk.texture = tc.texture
ARRAY JOIN [substring(if(tc.lump >= 0, l.bytes, tk.data), tc.ofs + 1, 128)] AS src;

-- The posts of every texture a two-sided line draws in the middle.
-- `R_RenderMaskedSegRange` steps back three bytes from the column's own
-- offset and reads a post chain from there, so that is where the walk
-- starts and what it reads is what the engine reads.
INSERT INTO {{DB}}.tex_posts (texture, col, idx, topdelta, length, data)
SELECT
    w.texture,
    w.col,
    toUInt16(n - 1) AS idx,
    post.1 AS topdelta,
    post.2 AS length,
    substring(w.src, post.3 + 1, post.2) AS data
FROM
(
    SELECT
        tc.texture AS texture,
        tc.col AS col,
        if(tc.lump >= 0, l.bytes, tk.data) AS src,
        arrayFold(
            (acc, k) -> if(
                acc.1 >= toUInt32(length(src))
                    OR reinterpretAsUInt8(substring(src, acc.1 + 1, 1)) = 255,
                acc,
                (toUInt32(acc.1 + reinterpretAsUInt8(substring(src, acc.1 + 2, 1)) + 4),
                 arrayPushBack(acc.2, (reinterpretAsUInt8(substring(src, acc.1 + 1, 1)),
                                       reinterpretAsUInt8(substring(src, acc.1 + 2, 1)),
                                       toUInt32(acc.1 + 3))))),
            range(64),
            (toUInt32(greatest(toInt64(tc.ofs) - 3, 0)),
             CAST([], 'Array(Tuple(UInt8, UInt8, UInt32))'))).2 AS posts
    FROM {{DB}}.tex_columns AS tc
    LEFT JOIN {{DB}}.wad_lumps AS l ON l.id = toUInt32(greatest(tc.lump, 0))
    LEFT JOIN {{DB}}.tex_composite AS tk ON tk.texture = tc.texture
    WHERE tc.texture IN
    (
        SELECT toUInt32(sd.midtexture)
        FROM {{DB}}.lv_lines AS ld
        INNER JOIN {{DB}}.lv_sides AS sd ON sd.id = toUInt32(ld.side0) OR sd.id = toUInt32(ld.side1)
        WHERE bitAnd(ld.flags, 4) != 0 AND sd.midtexture > 0
    )
) AS w
ARRAY JOIN w.posts AS post, arrayEnumerate(w.posts) AS n;

-- ---------------------------------------------------------------------------
-- Flats, colormaps and the palette
-- ---------------------------------------------------------------------------

-- `R_InitFlats` numbers flats from `F_START + 1` to `F_END - 1`, so the two
-- inner markers take flat numbers of their own and carry no pixels.
INSERT INTO {{DB}}.flats (id, name, data)
SELECT
    l.id - m.first AS id,
    l.name,
    l.bytes AS data
FROM {{DB}}.wad_lumps AS l
CROSS JOIN
(
    SELECT
        (SELECT id FROM {{DB}}.wad_lumps WHERE name = 'F_START') + 1 AS first,
        (SELECT id FROM {{DB}}.wad_lumps WHERE name = 'F_END') - 1 AS last
) AS m
WHERE l.id >= m.first AND l.id <= m.last;

-- COLORMAP: 34 tables of 256 bytes.
INSERT INTO {{DB}}.colormap (id, data)
SELECT toUInt8(i) AS id, substring(l.bytes, i * 256 + 1, 256) AS data
FROM {{DB}}.wad_lumps AS l
ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 256))) AS i
WHERE l.name = 'COLORMAP';

-- PLAYPAL: 14 palettes of 256 RGB triples.
INSERT INTO {{DB}}.playpal (id, data)
SELECT toUInt8(i) AS id, substring(l.bytes, i * 768 + 1, 768) AS data
FROM {{DB}}.wad_lumps AS l
ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 768))) AS i
WHERE l.name = 'PLAYPAL';

-- ---------------------------------------------------------------------------
-- Sprites
-- ---------------------------------------------------------------------------

-- `R_InitSpriteLumps` numbers sprite lumps from the one after `S_START`
-- and keeps the width and the two offsets in `fixed_t`.
INSERT INTO {{DB}}.sprite_lumps (id, lump, name, width, height, width_fixed, leftoffset, topoffset)
SELECT
    p.lump - m.first AS id,
    p.lump AS lump,
    p.name AS name,
    p.width AS width,
    p.height AS height,
    toInt32(p.width) * 65536 AS width_fixed,
    toInt32(p.leftoffset) * 65536 AS leftoffset,
    toInt32(p.topoffset) * 65536 AS topoffset
FROM {{DB}}.patch_lumps AS p
CROSS JOIN
(
    SELECT
        (SELECT id FROM {{DB}}.wad_lumps WHERE name = 'S_START') + 1 AS first,
        (SELECT id FROM {{DB}}.wad_lumps WHERE name = 'S_END') - 1 AS last
) AS m
WHERE p.lump >= m.first AND p.lump <= m.last;

-- `R_InitSpriteDefs` reads a frame letter and a rotation digit out of a
-- lump's name, and a second pair out of the last two characters when the
-- name is eight long, which names the same picture flipped. Rotation 0
-- serves all eight, and a later lump wins a slot an earlier one filled.
INSERT INTO {{DB}}.sprite_frames (sprite, frame, rotate, lump, flip)
SELECT
    sprite,
    frame,
    toUInt8(min(rotation) != 0) AS rotate,
    arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((slot, lump)))) AS lump,
    arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((slot, flip)))) AS flip
FROM
(
    SELECT
        sprite,
        frame,
        slot,
        argMax(sl.id, sl.id) AS lump,
        argMax(flipped, sl.id) AS flip,
        argMax(rotation, sl.id) AS rotation
    FROM
    (
        SELECT
            sn.id AS sprite,
            toUInt8(reinterpretAsUInt8(substring(sl.name, 5 + h * 2, 1)) - 65) AS frame,
            toUInt8(reinterpretAsUInt8(substring(sl.name, 6 + h * 2, 1)) - 48) AS rotation,
            toUInt8(h) AS flipped,
            sl.id AS id
        FROM {{DB}}.sprite_lumps AS sl
        INNER JOIN {{DB}}.sprnames AS sn ON sn.name = substring(sl.name, 1, 4)
        ARRAY JOIN range(2) AS h
        WHERE h = 0 OR length(sl.name) = 8
    ) AS e
    ARRAY JOIN if(e.rotation = 0, range(8), [toUInt32(e.rotation) - 1]) AS slot
    INNER JOIN {{DB}}.sprite_lumps AS sl ON sl.id = e.id
    GROUP BY sprite, frame, slot
)
GROUP BY sprite, frame;

-- A frame the engine would refuse: one that mixes a rotation-0 lump with
-- rotated ones, or that leaves a rotation with no picture.
SELECT throwIf(count() > 0, 'a sprite frame does not cover its rotations')
FROM {{DB}}.sprite_frames
WHERE length(lump) != 8 OR has(lump, -1);

-- Every sprite column's pixels end to end, and where each post's pixels
-- sit in it.
INSERT INTO {{DB}}.sprite_posts (lump, col, idx, topdelta, length, pool_ofs)
SELECT
    sl.id AS lump,
    q.col AS col,
    q.idx AS idx,
    q.topdelta AS topdelta,
    q.length AS length,
    toUInt32(sum(q.length) OVER (ORDER BY sl.id, q.col, q.idx
                                 ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)) AS pool_ofs
FROM {{DB}}.patch_posts AS q
INNER JOIN {{DB}}.sprite_lumps AS sl ON sl.lump = q.lump;

INSERT INTO {{DB}}.sprite_pool (id, data)
SELECT 0 AS id, arrayStringConcat(arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((key, data)))), '') AS data
FROM
(
    SELECT
        (toUInt64(sl.id) * 65536 + toUInt64(q.col)) * 256 + q.idx AS key,
        substring(l.bytes, q.ofs + 1, q.length) AS data
    FROM {{DB}}.patch_posts AS q
    INNER JOIN {{DB}}.sprite_lumps AS sl ON sl.lump = q.lump
    INNER JOIN {{DB}}.wad_lumps AS l ON l.id = q.lump
);

-- Every post's pixels have to sit at the offset the table gives, or a
-- sprite draw reads the post before or after it.
SELECT throwIf(count() > 0, 'the sprite pool is not as long as its posts')
FROM {{DB}}.sprite_pool AS p
CROSS JOIN (SELECT max(pool_ofs + length) AS want FROM {{DB}}.sprite_posts) AS s
WHERE length(p.data) != s.want;

-- ---------------------------------------------------------------------------
-- The status bar, font and menu graphics
-- ---------------------------------------------------------------------------

-- `ST_loadUnloadGraphics` and `HU_Init` name these by lump. They are read
-- whole rather than by column, so the row carries the lump's own bytes.
INSERT INTO {{DB}}.ui_patches (id, name, width, height, leftoffset, topoffset, columnofs, data)
SELECT p.lump AS id, p.name, p.width, p.height, p.leftoffset, p.topoffset, p.columnofs, l.bytes AS data
FROM {{DB}}.patch_lumps AS p
INNER JOIN {{DB}}.wad_lumps AS l ON l.id = p.lump
WHERE p.name IN ('STBAR', 'STARMS', 'STTMINUS', 'STTPRCNT')
   OR p.name LIKE 'STTNUM%' OR p.name LIKE 'STYSNUM%' OR p.name LIKE 'STGNUM%'
   OR p.name LIKE 'STKEYS%' OR p.name LIKE 'STF%' OR p.name LIKE 'STCFN%';

-- ---------------------------------------------------------------------------
-- Map geometry
-- ---------------------------------------------------------------------------

INSERT INTO {{DB}}.lv_vertexes (id, x, y)
SELECT
    i AS id,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 4 + 1, 2))) AS x,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 4 + 3, 2))) AS y
FROM {{DB}}.wad_lumps AS l
ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 4))) AS i
WHERE l.map_marker = '{{MAP}}' AND l.name = 'VERTEXES';

-- `R_CheckTextureNumForName` reads a name starting with `-` as texture 0,
-- the no-texture marker, and resolves everything else by name.
INSERT INTO {{DB}}.lv_sides
    (id, textureoffset, rowoffset, toptexture, bottomtexture, midtexture, sector)
SELECT
    i AS id,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 30 + 1, 2))) AS textureoffset,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 30 + 3, 2))) AS rowoffset,
    toInt16(t.m[upper(splitByChar('\0', substring(l.bytes, i * 30 + 5, 8))[1])]) AS toptexture,
    toInt16(t.m[upper(splitByChar('\0', substring(l.bytes, i * 30 + 13, 8))[1])]) AS bottomtexture,
    toInt16(t.m[upper(splitByChar('\0', substring(l.bytes, i * 30 + 21, 8))[1])]) AS midtexture,
    reinterpretAsUInt16(substring(l.bytes, i * 30 + 29, 2)) AS sector
FROM {{DB}}.wad_lumps AS l
CROSS JOIN
(
    -- `-` names no texture and reads as 0, which is also what an entry
    -- this map never uses reads as.
    SELECT mapFromArrays(arrayPushBack(groupArray(name), '-'),
                         arrayPushBack(groupArray(id), toUInt32(0))) AS m
    FROM {{DB}}.tex_textures
) AS t
ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 30))) AS i
WHERE l.map_marker = '{{MAP}}' AND l.name = 'SIDEDEFS';

-- A texture name the WAD does not define would have read as texture 0 and
-- drawn nothing.
SELECT throwIf(count() > 0, 'a sidedef names a texture TEXTURE1 does not define')
FROM
(
    SELECT upper(splitByChar('\0', substring(l.bytes, i * 30 + 5 + k * 8, 8))[1]) AS name
    FROM {{DB}}.wad_lumps AS l
    ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 30))) AS i
    ARRAY JOIN range(3) AS k
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'SIDEDEFS'
)
WHERE name != '-' AND name NOT IN (SELECT name FROM {{DB}}.tex_textures);

-- `dx`, `dy`, `slopetype` and `bbox` are what `P_LoadLineDefs` derives from
-- the two vertices. `sidenum[1]` is -1 on a one-sided line, and the sector
-- behind it is -1 with it.
INSERT INTO {{DB}}.lv_lines
    (id, v1, v2, dx, dy, flags, special, tag, slopetype, bbox, side0, side1, sector0, sector1)
SELECT
    x.id,
    x.v1,
    x.v2,
    toInt32(vtx.mx[x.v2] - vtx.mx[x.v1]) AS dx,
    toInt32(vtx.my[x.v2] - vtx.my[x.v1]) AS dy,
    x.flags,
    x.special,
    x.tag,
    toUInt8(multiIf(dx = 0, 1, dy = 0, 0, (dx > 0) = (dy > 0), 2, 3)) AS slopetype,
    [greatest(vtx.my[x.v1], vtx.my[x.v2]), least(vtx.my[x.v1], vtx.my[x.v2]),
     least(vtx.mx[x.v1], vtx.mx[x.v2]), greatest(vtx.mx[x.v1], vtx.mx[x.v2])] AS bbox,
    toInt32(x.side0) AS side0,
    toInt32(x.side1) AS side1,
    if(x.side0 < 0, -1, toInt32(sd.m[toUInt32(x.side0)])) AS sector0,
    if(x.side1 < 0, -1, toInt32(sd.m[toUInt32(x.side1)])) AS sector1
FROM
(
    SELECT
        i AS id,
        toUInt32(reinterpretAsUInt16(substring(l.bytes, i * 14 + 1, 2))) AS v1,
        toUInt32(reinterpretAsUInt16(substring(l.bytes, i * 14 + 3, 2))) AS v2,
        reinterpretAsInt16(substring(l.bytes, i * 14 + 5, 2)) AS flags,
        reinterpretAsInt16(substring(l.bytes, i * 14 + 7, 2)) AS special,
        reinterpretAsInt16(substring(l.bytes, i * 14 + 9, 2)) AS tag,
        reinterpretAsInt16(substring(l.bytes, i * 14 + 11, 2)) AS side0,
        reinterpretAsInt16(substring(l.bytes, i * 14 + 13, 2)) AS side1
    FROM {{DB}}.wad_lumps AS l
    ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 14))) AS i
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'LINEDEFS'
) AS x
CROSS JOIN
(
    SELECT
        mapFromArrays(groupArray(id), groupArray(x)) AS mx,
        mapFromArrays(groupArray(id), groupArray(y)) AS my
    FROM {{DB}}.lv_vertexes
) AS vtx
CROSS JOIN
(
    SELECT mapFromArrays(groupArray(id), groupArray(sector)) AS m FROM {{DB}}.lv_sides
) AS sd;

-- `P_GroupLines` gives each sector the lines that touch it, in line order,
-- then the bounding box over those lines and the sound origin at its
-- centre. It runs in the same statement as the decode, so the table takes
-- one write.
INSERT INTO {{DB}}.lv_sectors_static
    (id, floorheight, ceilingheight, floorpic, ceilingpic, lightlevel, special, tag,
     blockbox, soundorg_x, soundorg_y, lines)
SELECT
    s.id,
    s.floorheight,
    s.ceilingheight,
    s.floorpic,
    s.ceilingpic,
    s.lightlevel,
    s.special,
    s.tag,
    [g.top, g.bottom, g.left, g.right] AS blockbox,
    toInt32(intDiv(toInt64(g.left) + toInt64(g.right), 2)) AS soundorg_x,
    toInt32(intDiv(toInt64(g.top) + toInt64(g.bottom), 2)) AS soundorg_y,
    g.lines
FROM
(
    SELECT
        i AS id,
        reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 26 + 1, 2))) AS floorheight,
        reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 26 + 3, 2))) AS ceilingheight,
        toInt16(f.m[upper(splitByChar('\0', substring(l.bytes, i * 26 + 5, 8))[1])]) AS floorpic,
        toInt16(f.m[upper(splitByChar('\0', substring(l.bytes, i * 26 + 13, 8))[1])]) AS ceilingpic,
        reinterpretAsInt16(substring(l.bytes, i * 26 + 21, 2)) AS lightlevel,
        reinterpretAsInt16(substring(l.bytes, i * 26 + 23, 2)) AS special,
        reinterpretAsInt16(substring(l.bytes, i * 26 + 25, 2)) AS tag
    FROM {{DB}}.wad_lumps AS l
    CROSS JOIN
    (
        SELECT mapFromArrays(groupArray(upper(name)), groupArray(id)) AS m
        FROM {{DB}}.flats WHERE length(data) > 0
    ) AS f
    ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 26))) AS i
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'SECTORS'
) AS s
LEFT JOIN
(
    SELECT
        sector,
        arraySort(groupArray(id)) AS lines,
        max(bbox[1]) AS top,
        min(bbox[2]) AS bottom,
        min(bbox[3]) AS left,
        max(bbox[4]) AS right
    FROM
    (
        SELECT id, bbox, sector0 AS sector FROM {{DB}}.lv_lines
        UNION ALL
        SELECT id, bbox, sector1 AS sector FROM {{DB}}.lv_lines WHERE sector1 >= 0
    )
    GROUP BY sector
) AS g ON g.sector = toInt32(s.id);

-- A flat name the WAD does not hold would have read as flat 0.
SELECT throwIf(count() > 0, 'a sector names a flat the WAD does not hold')
FROM
(
    SELECT upper(splitByChar('\0', substring(l.bytes, i * 26 + 5 + k * 8, 8))[1]) AS name
    FROM {{DB}}.wad_lumps AS l
    ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 26))) AS i
    ARRAY JOIN range(2) AS k
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'SECTORS'
)
WHERE name NOT IN (SELECT upper(name) FROM {{DB}}.flats WHERE length(data) > 0);

INSERT INTO {{DB}}.lv_things (id, x, y, angle, type, options)
SELECT
    i AS id,
    reinterpretAsInt16(substring(l.bytes, i * 10 + 1, 2)) AS x,
    reinterpretAsInt16(substring(l.bytes, i * 10 + 3, 2)) AS y,
    reinterpretAsInt16(substring(l.bytes, i * 10 + 5, 2)) AS angle,
    reinterpretAsInt16(substring(l.bytes, i * 10 + 7, 2)) AS type,
    reinterpretAsInt16(substring(l.bytes, i * 10 + 9, 2)) AS options
FROM {{DB}}.wad_lumps AS l
ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 10))) AS i
WHERE l.map_marker = '{{MAP}}' AND l.name = 'THINGS';

-- `P_LoadSegs` takes the front sector from the side the seg is on, and the
-- back sector from the other side of the same line, but only when the line
-- carries ML_TWOSIDED.
INSERT INTO {{DB}}.lv_segs
    (id, v1, v2, offset, angle, sidedef, linedef, frontsector, backsector)
SELECT
    x.id,
    x.v1,
    x.v2,
    x.offset,
    x.angle,
    toUInt32(if(x.side = 0, ld.side0, ld.side1)) AS sidedef,
    x.linedef,
    if(x.side = 0, ld.sector0, ld.sector1) AS frontsector,
    multiIf(bitAnd(ld.flags, 4) = 0, -1, x.side = 0, ld.sector1, ld.sector0) AS backsector
FROM
(
    SELECT
        i AS id,
        reinterpretAsUInt16(substring(l.bytes, i * 12 + 1, 2)) AS v1,
        reinterpretAsUInt16(substring(l.bytes, i * 12 + 3, 2)) AS v2,
        reinterpretAsUInt32(concat('\0\0', substring(l.bytes, i * 12 + 5, 2))) AS angle,
        reinterpretAsUInt16(substring(l.bytes, i * 12 + 7, 2)) AS linedef,
        reinterpretAsInt16(substring(l.bytes, i * 12 + 9, 2)) AS side,
        reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 12 + 11, 2))) AS offset
    FROM {{DB}}.wad_lumps AS l
    ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 12))) AS i
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'SEGS'
) AS x
INNER JOIN {{DB}}.lv_lines AS ld ON ld.id = x.linedef;

-- `P_GroupLines` reads each subsector's sector off its first seg.
INSERT INTO {{DB}}.lv_subsectors (id, numlines, firstline, sector)
SELECT x.id, x.numlines, x.firstline, toUInt32(sg.frontsector) AS sector
FROM
(
    SELECT
        i AS id,
        reinterpretAsUInt16(substring(l.bytes, i * 4 + 1, 2)) AS numlines,
        reinterpretAsUInt16(substring(l.bytes, i * 4 + 3, 2)) AS firstline
    FROM {{DB}}.wad_lumps AS l
    ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 4))) AS i
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'SSECTORS'
) AS x
INNER JOIN {{DB}}.lv_segs AS sg ON sg.id = x.firstline;

-- `bbox` is the two children's boxes end to end, right's four then left's,
-- each top, bottom, left, right. `children` keeps the lump's own values,
-- bit 15 still marking a subsector.
INSERT INTO {{DB}}.lv_nodes (id, x, y, dx, dy, bbox, children)
SELECT
    i AS id,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 28 + 1, 2))) AS x,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 28 + 3, 2))) AS y,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 28 + 5, 2))) AS dx,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 28 + 7, 2))) AS dy,
    arrayMap(k -> reinterpretAsInt32(concat('\0\0', substring(l.bytes, i * 28 + 9 + k * 2, 2))),
             range(8)) AS bbox,
    [reinterpretAsUInt16(substring(l.bytes, i * 28 + 25, 2)),
     reinterpretAsUInt16(substring(l.bytes, i * 28 + 27, 2))] AS children
FROM {{DB}}.wad_lumps AS l
ARRAY JOIN range(toUInt32(intDiv(length(l.bytes), 28))) AS i
WHERE l.map_marker = '{{MAP}}' AND l.name = 'NODES';

INSERT INTO {{DB}}.lv_blockmap_header (origin_x, origin_y, columns, rows)
SELECT
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, 1, 2))) AS origin_x,
    reinterpretAsInt32(concat('\0\0', substring(l.bytes, 3, 2))) AS origin_y,
    reinterpretAsUInt16(substring(l.bytes, 5, 2)) AS columns,
    reinterpretAsUInt16(substring(l.bytes, 7, 2)) AS rows
FROM {{DB}}.wad_lumps AS l
WHERE l.map_marker = '{{MAP}}' AND l.name = 'BLOCKMAP';

-- `P_BlockLinesIterator` starts reading at the offset the header gives and
-- stops at the first -1. The word at that offset is a zero the format
-- always writes, and the engine reads it as line 0, so it stays in the
-- list.
INSERT INTO {{DB}}.lv_blockmap (cell, bx, by, lines)
SELECT
    c.cell,
    c.bx,
    c.by,
    arrayMap(k -> reinterpretAsUInt16(substring(c.bytes, (c.ofs + k) * 2 + 1, 2)),
             range(toUInt32(arrayFirstIndex(
                 k -> reinterpretAsInt16(substring(c.bytes, (c.ofs + k) * 2 + 1, 2)) = -1,
                 range(c.total - c.ofs)) - 1))) AS lines
FROM
(
    SELECT
        l.bytes AS bytes,
        toUInt32(intDiv(length(l.bytes), 2)) AS total,
        cell,
        cell % h.columns AS bx,
        intDiv(cell, h.columns) AS by,
        toUInt32(reinterpretAsUInt16(substring(l.bytes, (4 + cell) * 2 + 1, 2))) AS ofs
    FROM {{DB}}.wad_lumps AS l
    CROSS JOIN (SELECT columns, rows FROM {{DB}}.lv_blockmap_header LIMIT 1) AS h
    ARRAY JOIN range(toUInt32(h.columns * h.rows)) AS cell
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'BLOCKMAP'
) AS c
SETTINGS max_block_size = 256;

-- Every cell list has to end at a -1 inside the lump. A list that runs off
-- the end would have been read as an empty one.
SELECT throwIf(count() > 0, 'a blockmap cell list has no terminator')
FROM
(
    SELECT
        toUInt32(intDiv(length(l.bytes), 2)) AS total,
        toUInt32(reinterpretAsUInt16(substring(l.bytes, (4 + cell) * 2 + 1, 2))) AS ofs,
        arrayFirstIndex(
            k -> reinterpretAsInt16(substring(l.bytes, (ofs + k) * 2 + 1, 2)) = -1,
            range(total - ofs)) AS at
    FROM {{DB}}.wad_lumps AS l
    CROSS JOIN (SELECT columns, rows FROM {{DB}}.lv_blockmap_header LIMIT 1) AS h
    ARRAY JOIN range(toUInt32(h.columns * h.rows)) AS cell
    WHERE l.map_marker = '{{MAP}}' AND l.name = 'BLOCKMAP'
)
WHERE at = 0;

-- `P_LoadReject` reads REJECT as `numsectors * numsectors` bits packed end
-- to end, and pads it with zeros when the lump is shorter than that.
INSERT INTO {{DB}}.lv_reject (id, bits)
SELECT 0 AS id, rightPad(l.bytes, toUInt32(intDiv(s.n * s.n + 7, 8)), '\0') AS bits
FROM {{DB}}.wad_lumps AS l
CROSS JOIN (SELECT count() AS n FROM {{DB}}.lv_sectors_static) AS s
WHERE l.map_marker = '{{MAP}}' AND l.name = 'REJECT';

-- ---------------------------------------------------------------------------
-- The demo
-- ---------------------------------------------------------------------------

INSERT INTO {{DB}}.demo_header
    (id, version, skill, episode, map, deathmatch, respawn, fast, nomonsters, consoleplayer)
SELECT
    0 AS id,
    reinterpretAsUInt8(substring(l.bytes, 1, 1)) AS version,
    reinterpretAsUInt8(substring(l.bytes, 2, 1)) AS skill,
    reinterpretAsUInt8(substring(l.bytes, 3, 1)) AS episode,
    reinterpretAsUInt8(substring(l.bytes, 4, 1)) AS map,
    reinterpretAsUInt8(substring(l.bytes, 5, 1)) AS deathmatch,
    reinterpretAsUInt8(substring(l.bytes, 6, 1)) AS respawn,
    reinterpretAsUInt8(substring(l.bytes, 7, 1)) AS fast,
    reinterpretAsUInt8(substring(l.bytes, 8, 1)) AS nomonsters,
    reinterpretAsUInt8(substring(l.bytes, 9, 1)) AS consoleplayer
FROM {{DB}}.wad_lumps AS l
WHERE l.name = '{{DEMO}}';

-- The 13-byte header, then one four-byte tic command each, ending at the
-- 0x80 terminator. `G_ReadDemoTiccmd` scales the third byte by 256 into a
-- signed `angleturn`.
INSERT INTO {{DB}}.demo_cmds (tic, forwardmove, sidemove, angleturn, buttons)
SELECT
    i + 1 AS tic,
    reinterpretAsInt8(substring(l.bytes, 13 + i * 4 + 1, 1)) AS forwardmove,
    reinterpretAsInt8(substring(l.bytes, 13 + i * 4 + 2, 1)) AS sidemove,
    reinterpretAsInt16(concat('\0', substring(l.bytes, 13 + i * 4 + 3, 1))) AS angleturn,
    reinterpretAsUInt8(substring(l.bytes, 13 + i * 4 + 4, 1)) AS buttons
FROM {{DB}}.wad_lumps AS l
ARRAY JOIN range(toUInt32(intDiv(length(l.bytes) - 14, 4))) AS i
WHERE l.name = '{{DEMO}}';

-- ---------------------------------------------------------------------------
-- Derived geometry
-- ---------------------------------------------------------------------------

INSERT INTO {{DB}}.lv_sector_subsectors (sector, subsectors)
SELECT sector, arraySort(groupArray(id)) AS subsectors
FROM {{DB}}.lv_subsectors
GROUP BY sector;

-- The path from the BSP root to each subsector. `P_PointInSubsector` walks
-- it on every call; this holds it once. The root is the last node, and a
-- child with bit 15 set is a subsector.
--
-- `up.parent` and `up.side` are the parent of each node and the branch
-- that reaches it, indexed by node id; the root's parent is 4294967295.
-- The fold climbs from a subsector's own parent to the root, so the arrays
-- come out leaf first and are reversed.
INSERT INTO {{DB}}.lv_ssec_path (subsector, depth, nodes, sides)
SELECT
    subsector,
    toUInt32(length(walk.1)) AS depth,
    reverse(walk.1) AS nodes,
    reverse(walk.2) AS sides
FROM
(
    SELECT
        p.child AS subsector,
        arrayFold(
            (acc, step) -> if(acc.3 = 4294967295,
                              acc,
                              (arrayPushBack(acc.1, acc.3),
                               arrayPushBack(acc.2, up.side[acc.3 + 1]),
                               up.parent[acc.3 + 1])),
            range(64),
            (arrayPushBack(emptyArrayUInt32(), p.node),
             arrayPushBack(emptyArrayUInt8(), p.side),
             up.parent[p.node + 1])) AS walk
    FROM
    (
        SELECT
            n.id AS node,
            toUInt8(k) AS side,
            toUInt32(bitAnd(n.children[k + 1], 32767)) AS child
        FROM {{DB}}.lv_nodes AS n
        ARRAY JOIN range(2) AS k
        WHERE bitAnd(n.children[k + 1], 32768) != 0
    ) AS p
    CROSS JOIN
    (
        SELECT
            arrayMap(i -> toUInt32(if(indexOf(child_of, toUInt32(i)) = 0,
                                      4294967295,
                                      node_of[indexOf(child_of, toUInt32(i))])),
                     range(n)) AS parent,
            arrayMap(i -> toUInt8(if(indexOf(child_of, toUInt32(i)) = 0,
                                     0,
                                     side_of[indexOf(child_of, toUInt32(i))])),
                     range(n)) AS side
        FROM
        (
            SELECT
                (SELECT count() FROM {{DB}}.lv_nodes) AS n,
                groupArray(child) AS child_of,
                groupArray(node) AS node_of,
                groupArray(branch) AS side_of
            FROM
            (
                SELECT
                    n.id AS node,
                    toUInt8(k) AS branch,
                    toUInt32(bitAnd(n.children[k + 1], 32767)) AS child
                FROM {{DB}}.lv_nodes AS n
                ARRAY JOIN range(2) AS k
                WHERE bitAnd(n.children[k + 1], 32768) = 0
                ORDER BY node, branch
            )
        )
    ) AS up
);

-- Every subsector has to reach the root, or a traversal built from these
-- paths would visit a different tree than `R_RenderBSPNode` does.
SELECT throwIf(count() > 0, 'a subsector path does not start at the BSP root')
FROM {{DB}}.lv_ssec_path
WHERE nodes[1] != (SELECT max(id) FROM {{DB}}.lv_nodes);

-- Subsector ids ascend with the BSP build order, so the subsectors under a
-- node are a contiguous range.
INSERT INTO {{DB}}.lv_node_range (node, first_ssec, last_ssec)
SELECT node, min(subsector) AS first_ssec, max(subsector) AS last_ssec
FROM {{DB}}.lv_ssec_path
ARRAY JOIN nodes AS node
GROUP BY node;
