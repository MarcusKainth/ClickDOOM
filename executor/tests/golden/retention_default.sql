DELETE FROM clickdoom.batch_commit
WHERE batch_id < toUInt64(greatest(toInt64(0), toInt64(7) - 16))
SETTINGS lightweight_deletes_sync = 0