# hjkl-fuzzy

Subsequence fuzzy scorer for the [hjkl](https://hjkl.kryptic.sh) editor stack.

Given a `haystack` string and a `needle` pattern, returns an
`Option<(i64, Vec<usize>)>` — the relevance score and the **char indices** in
`haystack` where each needle character matched. Returns `None` when the needle
is not a subsequence of the haystack.

Bonuses applied: word-boundary (+8), consecutive run (+5 per continuation), base
hit (+1). Penalty: `−len(haystack)/8` so shorter paths win on ties. A literal
substring match is always boosted above any scattered subsequence.

```rust
use hjkl_fuzzy::score;

// Full substring match — positions are contiguous.
let (s, pos) = score("/home/user/project/main.rs", "main").unwrap();
assert_eq!(pos, vec![19, 20, 21, 22]);

// No match.
assert!(score("src/lib.rs", "xyz").is_none());
```
