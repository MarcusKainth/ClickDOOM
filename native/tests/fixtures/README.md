# native/tests/fixtures/

What the renderer is checked against: one game state, and the two frames the
real engine drew around it.

Everything here comes from the reference emulator running
`rom/build/doom-rv32im.elf`, whose flattened segments hash to
`9a6a47d01119f67580e48e9875207186c25efd56ff93019df331eb307cfaa5d9`.

Five frames are covered. Frames 0 and 20 are the first and the middle of the
screen melt, both drawn from gametic 2 over a black screen. Frame 40 is the
first frame after the melt, drawn from gametic 3: a lit room with two things in it and the weapon off
the bottom of the view. Frame 110 is the first frame the Spectre is visible in,
so it is the one that draws a thing as a shadow. Frame 1000 is drawn from
gametic 963, with the shotgun raised and the player somewhere else on the map.

| File | What it is |
|---|---|
| `demo3-states.tsv` | Four rows in the shape `refemu/probe/README.md` describes: frame index, gametic, frame hash, then every field of `clickdoom_spec::native_state`. |
| `demo3-frame0-*.bin`, `demo3-frame20-*.bin` | Two frames of the melt, which draws over a black screen rather than over the frame before it. |
| `demo3-frame39-fb.bin`, `demo3-frame39-palette.bin` | The 64,000 bytes and 768-byte palette of frame 39, which frame 40 draws over. |
| `demo3-frame40-fb.bin`, `demo3-frame40-palette.bin` | The same for frame 40, which the render test compares against. |
| `demo3-frame109-*.bin`, `demo3-frame110-*.bin` | The same pair around frame 110. |
| `demo3-frame999-*.bin`, `demo3-frame1000-*.bin` | The same pair around frame 1000. |

The two framebuffers come from

    refemu run rom/build/doom-rv32im.elf --manifest rom/build/manifest.json \
        -n 2000000000 --stop-at frame:N --dump-frame OUT.snap

taking the `framebuffer` and `palette` sections of the snapshot. The state rows
are two rows of the probe's own demo3 fixture.

`xxHash64(framebuffer || palette)` is `fe5d82c0f42d45f1` for frame 0,
`5609b242d753d5d6` for frame 20, `cd922a65a5e95c23` for frame 39,
`2eb87849ee6d9714` for frame 40, `0efada37fbd0c792` for frame 109,
`ffca3225ffc14b77` for frame 110, `0fe27f3c06fb13ba` for frame 999 and
`01539e688afb3350` for frame 1000. The test checks all eight before it uses
them, so a fixture that was replaced by something else fails rather than
passing quietly.
