-- Isolated fixture for the render tests. Written
-- against sqlcpu's #130 design comment for framebuffer/palette, confirmed
-- with sqlcpu-2 before use (issue #29's plan comment) -- #174 has since
-- landed that exact shape for real in sqlcpu/schema.sql (framebuffer,
-- palette, and batch_commit's six fb_wl_*/pal_wl_* columns all match
-- byte-for-byte). This file still exists as a fast, isolated fixture for
-- the render tests rather than requiring the real, shared schema.
--
-- {{DB}} is substituted by the caller (same convention as sqlcpu/
-- schema.sql's `clickdoom` placeholder database name, substituted the
-- same way preflight_milestone.sh/run_milestone.sh already do for their
-- own throwaway databases).

CREATE DATABASE IF NOT EXISTS {{DB}};

CREATE TABLE {{DB}}.framebuffer
(
    spec_version String DEFAULT '0.2.0',
    word_addr    UInt32,  -- relative to FRAMEBUFFER_BASE (0x1100_0000), 0..15999
    value        UInt32,
    version      UInt64   -- the store's absolute icount, same convention as ram
)
ENGINE = ReplacingMergeTree(version)
ORDER BY word_addr;

CREATE TABLE {{DB}}.palette
(
    spec_version String DEFAULT '0.2.0',
    word_addr    UInt32,  -- relative to PALETTE_BASE (0x1101_0000), 0..191
    value        UInt32,
    version      UInt64
)
ENGINE = ReplacingMergeTree(version)
ORDER BY word_addr;

-- frames_out and batch_commit's has_frame/frame_no/icount columns: copied
-- from sqlcpu/schema.sql's real DDL verbatim (not reinvented), since
-- frame_readout_sql() reads batch_commit and writes
-- frames_out exactly as the real schema defines them.
CREATE TABLE {{DB}}.frames_out
(
    spec_version     String DEFAULT '0.2.0',
    frame_no         UInt32,
    committed_icount UInt64,
    fb               String,  -- 64,000 bytes: 320x200, 8bpp palette-indexed, row-major
    palette          String   -- 768 bytes: 256 x RGB (3 bytes each)
)
ENGINE = MergeTree
ORDER BY frame_no;

CREATE TABLE {{DB}}.batch_commit
(
    spec_version String DEFAULT '0.2.0',
    batch_id     UInt64,
    icount       UInt64,
    pc           UInt32,
    regs         Array(UInt32),
    halted       UInt8,
    halt_reason  LowCardinality(String),
    exit_code    UInt32,
    keyq_pos     UInt64,
    has_frame    UInt8,
    frame_no     UInt32,
    wl_addr      Array(UInt32),
    wl_val       Array(UInt32),
    wl_icount    Array(UInt64),
    -- #174's real six FRAMEBUFFER/PALETTE write-log columns -- added here
    -- so this fixture stays a byte-exact mirror of sqlcpu/schema.sql now
    -- that #174 landed, even though frame_readout_sql() doesn't read them
    -- (it reads framebuffer/palette directly, not via this write-log).
    -- Omitted from seed_frame_fixture.py's explicit-column INSERT, same as
    -- wl_addr/wl_val/wl_icount already were -- ClickHouse fills an
    -- unlisted Array column with [] by default, which is the right seed
    -- value here regardless.
    fb_wl_addr    Array(UInt32),
    fb_wl_val     Array(UInt32),
    fb_wl_icount  Array(UInt64),
    pal_wl_addr   Array(UInt32),
    pal_wl_val    Array(UInt32),
    pal_wl_icount Array(UInt64),
    console_bytes Array(UInt8)
)
ENGINE = MergeTree
ORDER BY batch_id;
