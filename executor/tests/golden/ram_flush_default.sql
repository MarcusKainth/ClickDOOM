INSERT INTO clickdoom.ram (word_addr, value, version)
SELECT 536870912 + t.1, t.2, t.3
FROM (
    SELECT arrayJoin(arrayZip(wl_addr, wl_val, wl_icount)) AS t
    FROM clickdoom.batch_commit
    WHERE batch_id = (SELECT max(batch_id) FROM clickdoom.batch_commit)
)