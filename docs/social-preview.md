# Social preview

`social-preview.png` is the image GitHub shows when the repository is linked
from elsewhere, uploaded by hand under the repository's settings. It is
1280×640, the size GitHub asks for, and every pixel in it is one the native
renderer drew.

## Provenance

The frame is frame 420 of `demo3`, drawn by `clickdoom native demo` from the
game state the reference emulator recorded at that frame, on the ROM whose
flattened segments hash to the value in `rom/PINNED_HASH`. Its frame hash is
`2c988cfb72a7bfab`, the same as the engine's own frame.

The image is the top 320×160 of the 320×200 frame, scaled four times with no
smoothing.

## Rebuilding it

    make native-load
    clickdoom native demo demo3 --no-window --stop-at-frame 420 --frame-dir frames --database clickdoom_native
    ffmpeg -i frames/frame-00420.ppm -vf "crop=320:160:0:0,scale=1280:640:flags=neighbor" docs/social-preview.png
