DELETE FROM clickdoom.batch_commit
WHERE batch_id < toUInt64(greatest(toInt64(0), toInt64((SELECT max(batch_id) FROM clickdoom.batch_commit)) - 16))
SETTINGS lightweight_deletes_sync = 0