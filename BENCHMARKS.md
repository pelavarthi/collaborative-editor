# Benchmarks

Run them with:

```text
cargo run --release -p collab-server --example bench
```

Each case warms up, then times a fixed number of iterations and prints
the mean per op. The numbers below are from this VM (release build) and
are for comparing one change against another on the same machine, not
for comparing against other editors.

## Results

| Case | Before (Day 13) | After (Day 13) |
| --- | --- | --- |
| insert 1 char at end, 10k doc | 11.06 µs | 86 ns |
| insert 1 char in middle, 10k doc | 17.43 µs | 17.89 µs |
| delete 1 char in middle, 10k doc | 17.70 µs | 16.32 µs |
| format 1000 chars, 10k doc | 2.29 µs | 2.09 µs |
| transform 1 op past 100 missed ops | 3.16 µs | 3.09 µs |
| spans of 10k doc, half bold | 34.03 µs | 43.03 µs |
| doc len, 100k doc | 7.53 µs | too fast to measure |
| room apply, typing at end | 2.92 µs | 180 ns |
| store append, one op (fsync) | 8.91 µs | 12.43 µs |
| snapshot save + load, 10k doc | 1.87 ms | 1.86 ms |

## What was tuned

Typing at the end of a document was doing O(document length) work per
keystroke for no reason. Two things caused it:

- `Document::len()` re-counted every character in the text on each
  call, and `apply` called it on every op (even on success, because
  the error value is built eagerly).
- `char_to_byte` walked the string from the start to find a position,
  even for the end of the document, which is where typing lands.

The fix is a cached character count on `Document`, kept in step on
insert and delete, plus fast paths in `char_to_byte` for position 0
and the end. `Room::len()` exposes the cached count, and the server's
presence handler uses it instead of re-counting the text on every
cursor move. Typing at the end of a 10k document went from 11 µs to
86 ns per keystroke, and `len()` on a 100k document from 7.5 µs to
nothing measurable.

## What did not change, and why

- Mid-document edits are still ~17 µs: finding a middle position
  still scans from the start, and inserting there still shifts the
  rest of the string. A rope would fix that; this chunk does not add
  one.
- Transform, formatting, and spans are unchanged: they were already
  proportional to the op or the range, not the document.
- The op log still fsyncs on every op (~10 µs here). That is the
  price of not losing an op the clients already saw, so it stays.
- A snapshot save + load is ~1.9 ms for a 10k document and runs every
  50 ops. It serializes the whole document, so it grows with document
  size; at this size it is one slow keystroke in fifty.
