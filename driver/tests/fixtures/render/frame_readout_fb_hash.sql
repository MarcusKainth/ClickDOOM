SELECT lpad(lower(hex(xxHash64(concat(fb, palette)))), 16, '0') AS fbhash
FROM (SELECT fb, palette FROM db1.frames_out ORDER BY frame_no DESC LIMIT 1)