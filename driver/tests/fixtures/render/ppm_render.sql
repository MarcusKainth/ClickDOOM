WITH (SELECT fb FROM db1.frames_out ORDER BY frame_no DESC LIMIT 1) AS fb, (SELECT palette FROM db1.frames_out ORDER BY frame_no DESC LIMIT 1) AS palette
SELECT concat('P6
4 2
255
', arrayStringConcat(arrayMap(
  i -> substring(palette, 3 * reinterpretAsUInt8(substring(fb, i, 1)) + 1, 3),
  range(1, 8 + 1)), '')) AS ppm