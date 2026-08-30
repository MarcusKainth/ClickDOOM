SELECT concat('P6
4 2
255
', unhex(arrayStringConcat(arrayMap(i -> concat(hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].1))),hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].2))),hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].3)))), range(1, 8 + 1))))) AS ppm
FROM
  (SELECT arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, 8 + 1)) AS px
   FROM (SELECT fb FROM db1.frames_out ORDER BY frame_no DESC LIMIT 1)) AS pixels,
  (SELECT arrayMap(i -> tuple(
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 1, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 2, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 3, 1))
    ), range(1, 257)) AS pal_rgb
   FROM (SELECT palette FROM db1.frames_out ORDER BY frame_no DESC LIMIT 1)) AS palettes