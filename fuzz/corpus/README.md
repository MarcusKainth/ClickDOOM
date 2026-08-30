# fuzz/corpus/

Cases a target found something with, kept so the finding runs from then on
whether or not anyone fuzzes again.

Not libFuzzer's own exploration. That is thousands of files, it regenerates
them, and none of them documents anything.

Add a case here once it is understood, named after what it exercises, and put
the fix's own test beside the code it fixes. The case is the belt; the test is
the braces.

## `elf_loader/segment-at-the-top-of-the-address-space`

An ELF declaring an executable segment whose address range runs off the end of
the address space. It parsed, and the read-only region it implied then came
out empty, so a store the machine should have refused would have landed.
