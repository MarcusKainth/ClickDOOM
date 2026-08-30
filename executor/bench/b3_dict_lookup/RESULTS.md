# B3 results: dictGet against arrayElement for RAM reads inside arrayFold

Harness and how to rerun: [README.md](README.md).

## Results, 2026-08-29, best of three

| variant | seconds |
|---|---|
| floor, K=20,000 | 0.090 |
| capture the array only, K=1 | 0.128 |
| arrayElement, one read per step | 0.264 |
| dictGet FLAT, one read per step | 0.166 |
| dictGet HASHED, one read per step | 0.160 |
| arrayElement, four reads per step | 0.394 |
| dictGet FLAT, four reads per step | 0.329 |
| `SYSTEM RELOAD DICTIONARY` after 20,000 stores | 0.064 |
| capture the array after 20,000 stores, K=1 | 0.183 |

Per read, after subtracting the floor and the capture: arrayElement about 2.2 us, dictGet about 3.0 to 3.8 us.
Per batch, the dictionary saves the capture (0.12 to 0.18 s) and costs a reload (0.06 s).

## Reading

A read through a dictionary is 1.4x slower per node than a read from the captured array. The dictionary's fixed cost per batch is 0.12 s lower.
At 60,000 steps and about one RAM read per step the two effects are within 0.1 s of each other, under 1% of a 16 s batch either way.

## Decision

No lever. Dictionaries change where a lookup node's time goes, not how much of it there is. The fold keeps the captured array.
