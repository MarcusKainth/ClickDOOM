INSERT INTO clickdoom.framebuffer (word_addr, value, version)
SELECT t.1, t.2, t.3
FROM (
    SELECT arrayJoin(arrayZip(fb_wl_addr, fb_wl_val, fb_wl_icount)) AS t
    FROM clickdoom.batch_commit
    WHERE batch_id = 7
);
INSERT INTO clickdoom.palette (word_addr, value, version)
SELECT t.1, t.2, t.3
FROM (
    SELECT arrayJoin(arrayZip(pal_wl_addr, pal_wl_val, pal_wl_icount)) AS t
    FROM clickdoom.batch_commit
    WHERE batch_id = 7
)