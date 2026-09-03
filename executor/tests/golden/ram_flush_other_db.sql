INSERT INTO other_db.ram (word_addr, value, version)
SELECT 536870912 + t.1, t.2, t.3
FROM (
    SELECT arrayJoin(arrayZip(wl_addr, wl_val, wl_icount)) AS t
    FROM other_db.batch_commit
    WHERE batch_id = 7
)