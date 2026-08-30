INSERT INTO db1.frames_out (frame_no, committed_icount, fb, palette)
SELECT frame_no, icount, unhex(arrayStringConcat(arrayMap(w -> hex(reinterpretAsFixedString(toUInt32(w))), (SELECT groupArray(value) FROM (SELECT coalesce(t.value, 0) AS value FROM (SELECT number AS word_addr FROM numbers(16000)) n LEFT JOIN (SELECT word_addr, value FROM db1.framebuffer FINAL) t ON n.word_addr = t.word_addr ORDER BY n.word_addr))))) AS fb, unhex(arrayStringConcat(arrayMap(w -> hex(reinterpretAsFixedString(toUInt32(w))), (SELECT groupArray(value) FROM (SELECT coalesce(t.value, 0) AS value FROM (SELECT number AS word_addr FROM numbers(192)) n LEFT JOIN (SELECT word_addr, value FROM db1.palette FINAL) t ON n.word_addr = t.word_addr ORDER BY n.word_addr))))) AS palette
FROM (
    SELECT frame_no, icount
    FROM db1.batch_commit
    WHERE has_frame = 1
    ORDER BY batch_id DESC
    LIMIT 1
)