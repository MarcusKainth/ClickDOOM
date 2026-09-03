INSERT INTO clickdoom.console_out (seq, byte)
SELECT bitShiftLeft(bc.batch_id, 32) + (t.1 - 1), t.2
FROM (
    SELECT batch_id, arrayJoin(arrayZip(arrayEnumerate(console_bytes), console_bytes)) AS t
    FROM clickdoom.batch_commit
    WHERE batch_id = 7
) AS bc