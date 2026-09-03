# Throughput at the frame 200 window

The canonical throughput instrument's gameplay window moved to start on
demo3's frame 200 commit, and the fold gained the register checkpoints it
records inside a batch. This measures what the two windows read afterwards.

## Question

What does the real ROM run at, end to end and fold alone, on the boot window
and on a gameplay window that starts where frame 200 retires, on the pinned
ClickHouse, with the checkpoint recording in the fold?

## Method

`make bench-canonical-throughput` on commit a3f7be2, ClickHouse 26.7.5.10 at
the compose digest, K = 60,000, HWM = 20,000, one fresh container per arm,
four warm-up batches then three timed ones per arm, five repeats one after
another with a minute between them, the machine lock held throughout and no
other container or build running for the first four. Both arms of every
repeat end at the same pc and icount (gameplay: pc 0x80030d74, icount
195,003,691). Every timed gameplay batch retired the full K with a write log
of 5,917 to 6,498 entries against the 20,000 mark.

The gameplay window starts at icount 194,583,691, the instruction after frame
200's `FRAME_COMMIT` store, and frame 201 commits at 195,961,602, so the
window holds 1,377,911 instructions and the seven chained batches fit with
room.

## Numbers

Instructions per second, end to end and fold alone, per repeat:

| repeat | boot e2e | boot fold | gameplay e2e | gameplay fold |
|---|---|---|---|---|
| 1 | 5,116.8 | 5,123.2 | 4,904.9 | 4,853.8 |
| 2 | 5,084.6 | 5,123.1 | 4,862.2 | 4,853.2 |
| 3 | 5,161.4 | 5,201.7 | 4,853.1 | 4,814.8 |
| 4 | 5,137.1 | 5,072.4 | 4,880.3 | 4,898.1 |
| 5 | 5,119.3 | 5,060.7 | 3,177.1 | 4,738.5 |

Repeat 5's gameplay end-to-end arm ran while a build started on the machine,
which is what its 3,177 is; it is reported and left out of the means. Over
the other four gameplay arms and all five boot arms:

| window | end to end | fold alone |
|---|---|---|
| boot, from icount 0 | 5,124 +/- 28 | 5,116 +/- 54 |
| gameplay, from frame 200 | 4,875 +/- 23 | 4,855 +/- 34 |

## Verdict

Boot reads 5,124 instructions per second end to end, and the gameplay window
at frame 200 reads 4,875, which is 2.5% under the 5,000 bar SPEC §6 states.
Two things moved since the 5,060 recorded at the previous window: the window
itself, which starts 39 frames earlier and on a frame commit, and the
checkpoint recording inside the batch fold, measured at about 3.7% of the
fold when it landed. Which of the two accounts for how much is not separated
here.

Measured 3 September 2026.
