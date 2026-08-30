SELECT arrayStringConcat(
  arrayMap(
    r -> concat(
      arrayStringConcat(
        arrayMap(
          c -> concat(
            '[38;2;', toString(pal_rgb[px[r * 2 * 4 + c + 1] + 1].1),
            ';', toString(pal_rgb[px[r * 2 * 4 + c + 1] + 1].2),
            ';', toString(pal_rgb[px[r * 2 * 4 + c + 1] + 1].3), 'm',
            '[48;2;', toString(pal_rgb[px[(r * 2 + 1) * 4 + c + 1] + 1].1),
            ';', toString(pal_rgb[px[(r * 2 + 1) * 4 + c + 1] + 1].2),
            ';', toString(pal_rgb[px[(r * 2 + 1) * 4 + c + 1] + 1].3), 'm',
            '▀'
          ),
          range(0, 4)
        )
      ),
      '[0m'
    ),
    range(0, 1)
  ),
  '
'
) AS ansi_frame
FROM
  (SELECT arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, 8 + 1)) AS px
   FROM (SELECT fb FROM db1.frames_out ORDER BY frame_no DESC LIMIT 1)) AS pixels,
  (SELECT arrayMap(i -> tuple(
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 1, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 2, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 3, 1))
    ), range(1, 257)) AS pal_rgb
   FROM (SELECT palette FROM db1.frames_out ORDER BY frame_no DESC LIMIT 1)) AS palettes