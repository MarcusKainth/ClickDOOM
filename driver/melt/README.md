# driver/melt/

How many passes of the screen melt the engine ran at each frame of a demo.
One TSV per demo, named after the demo lump in lower case. First line is the
column names, one line per melt frame after that, tab separated.

`native load` streams the file into `melt_schedule`, where SQL turns `passes`
into the running total the renderer reads as `melt_step`. A frame the file has
no row for is not a melt frame, and the driver feeds `melt_step = 0` for it.

## Where the numbers come from

`D_Display` calls `wipe_ScreenWipe` with the number of tics that passed since
the last call, so the melt advances by whatever the frame loop had time for.
That count is a property of the run, not of the demo lump, and the run is the
reference emulator's: `rom/build/doom-rv32im.elf`, whose flattened segments
hash to
`9a6a47d01119f67580e48e9875207186c25efd56ff93019df331eb307cfaa5d9`.

`demo3.tsv` is that run's, read off the frames the emulator dumped. Frame 0
advanced by one pass, frame 1 by two, and every frame after it by one. Frame
39 is the last frame the melt covers; frame 40 is the first gameplay frame,
which `refemu/probe/fixtures/demo3-frames.9a6a47d01119.json` names as
`first_gameplay_frame`.

## Checking a file

Render each of its frames from the probed states and compare the frame hash
against the probe's own:

    clickdoom native render --from probe --frame 39 --expect-fbhash cd922a65a5e95c23

A wrong pass count puts the melt's boundary in the wrong row and the hash
differs.

| File | Demo | Frames |
|---|---|---|
| `demo3.tsv` | `DEMO3`, E1M7 | 0 to 39 |
