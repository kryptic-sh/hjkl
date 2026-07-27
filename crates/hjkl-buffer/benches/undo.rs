//! Cold undo-tree node jump cost vs history depth — the regression guard for
//! the "keyframe materialization" item of issue #302.
//!
//! Storage (`src/undo.rs`, Phase 3a): the root holds a full base rope, every
//! other edge holds a reversible `Delta`, and a node's content is reconstructed
//! on demand behind a bounded warm LRU. `u` / `<C-r>` have an adjacent-node fast
//! path (one inverse delta apply), but `g-` / `g+` (`seq_earlier_step` /
//! `seq_later_step`) go through `entry_of` → `materialize`, which walks UP to
//! the nearest ancestor holding content and replays forward deltas. Before
//! keyframes that ancestor could be the root, making one jump O(depth) and a
//! full walk O(depth²); pinning a materialized rope every `KEYFRAME_INTERVAL`
//! nodes bounds a jump at O(K) and — since `materialize` also caches the
//! intermediates it replays — a full walk at O(N).
//!
//! The two benches below measure the two shapes that matter:
//!
//! - `cold_jump_back` — hold `g-` / `:earlier 9999`: walk tip → root.
//! - `single_deep_jump` — ONE `g-` deep inside the history (see [`WALKBACK`]
//!   for what that isolates before and after keyframes).
//!
//! Both are parameterized over history depth N so the scaling curve is visible.
//! History construction always happens in `iter_batched`'s SETUP closure, so it
//! is never inside the timed region; the timed closure only makes jump calls.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use hjkl_buffer::{Edit, MarkSnapshot, Position, UndoEntry, View};
use std::hint::black_box;
use std::time::{Duration, SystemTime};

/// Base document size: ~2000 lines of realistic prose-ish width.
const BASE_LINES: usize = 2_000;
const BASE_WIDTH: usize = 60;

/// History depths to chart. N = 16 is the shallow control point; the rest
/// progressively exceed the warm window (`WARM_CAP`).
const DEPTHS: [usize; 4] = [16, 64, 256, 1024];

/// How far back `single_deep_jump`'s setup walks before the timed jump.
///
/// Held at 20 so the number stays comparable across the keyframe change, but
/// note what it measures on each side. Before keyframes (`WARM_CAP = 16`, only
/// the jump TARGET cached) 20 steps left the warm LRU holding descendants only,
/// so the timed jump replayed the whole ancestor chain from the root base — the
/// cold-jump cost. Now `materialize` caches every intermediate it replays, so
/// walking back pre-warms the nodes ahead of the walk and no sequential `g-`
/// lands cold: the timed jump measures the steady-state per-step cost of holding
/// `g-` instead. The worst-case cold bound (`< KEYFRAME_INTERVAL` delta applies
/// from any node) is asserted directly in
/// `undo.rs::keyframes_bound_the_cold_replay_distance`, which is a sharper tool
/// for it than a wall-clock bench.
const WALKBACK: usize = 20;

fn base_text() -> String {
    let line: String = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(BASE_WIDTH)
        .collect();
    let mut text = String::with_capacity((BASE_WIDTH + 1) * BASE_LINES);
    for i in 0..BASE_LINES {
        text.push_str(&line);
        if i + 1 < BASE_LINES {
            text.push('\n');
        }
    }
    text
}

/// Build a `View` with a LINEAR undo history of `n` committed nodes.
///
/// One node per edit, using the only public commit path there is — the same
/// pre-edit-snapshot dance `Editor::push_undo_at` + `mutate_edit` performs in
/// `hjkl-engine`, and the same one the in-crate tests use
/// (`buffer.rs::undo_stack_shared_across_views`, `undo.rs::Driver::edit`):
/// push an `UndoEntry` holding the PRE-edit rope/cursor/marks, then mutate the
/// buffer. Timestamps are deterministic (`UNIX_EPOCH + i s`) like the tests'
/// fixtures, so no wall-clock jitter leaks into the history shape.
///
/// Each edit is small and realistic (a short marker inserted at the start of a
/// line), so each edge `Delta` is small — exactly the case keyframes would
/// target, since small deltas are what makes a long replay chain plausible.
fn build_history(base: &str, n: usize) -> View {
    let mut view = View::from_str(base);
    for i in 0..n {
        let cursor = view.cursor();
        view.push_undo_entry(UndoEntry {
            rope: view.rope(),
            cursor: (cursor.row, cursor.col),
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(i as u64),
            marks: MarkSnapshot::default(),
        });
        let row = i % BASE_LINES;
        view.apply_edit(Edit::InsertStr {
            at: Position::new(row, 0),
            text: format!("e{i} "),
        });
    }
    view
}

/// One `g-` step against `view`, mirroring `Editor::seq_earlier_core`: hand the
/// tree the LIVE rope (stashed into the node being left) and take the restored
/// snapshot back. Returns the restored rope, which becomes the next call's live
/// rope — i.e. what `restore_rope` would have put in the buffer.
fn earlier(view: &View, live: ropey::Rope) -> Option<ropey::Rope> {
    view.seq_earlier_step(live, (0, 0), MarkSnapshot::default())
        .map(|e| e.rope)
}

/// Hold `g-` from the tip all the way to the root.
///
/// Fresh deep-history `View` per iteration via `iter_batched` setup: walking
/// back mutates `current` and warms the LRU, so a reused tree would measure a
/// warm walk from iteration 2 onward.
fn bench_cold_jump_back(c: &mut Criterion) {
    let text = base_text();
    let mut group = c.benchmark_group("undo");
    for n in DEPTHS {
        group.bench_with_input(BenchmarkId::new("cold_jump_back", n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let view = build_history(&text, n);
                    let live = view.rope();
                    (view, live)
                },
                |(view, live)| {
                    let mut live = live;
                    let mut steps = 0usize;
                    while let Some(restored) = earlier(&view, live.clone()) {
                        live = restored;
                        steps += 1;
                    }
                    black_box(steps)
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

/// ONE `g-` deep inside a long history, after an untimed `WALKBACK`-step walk.
///
/// See [`WALKBACK`] for exactly what this measures before and after keyframes.
///
/// N = 16 is excluded: that whole history fits in the warm window, so there is
/// no deep jump to measure there — reporting that point would be a warm number
/// wearing a cold label.
fn bench_single_deep_jump(c: &mut Criterion) {
    let text = base_text();
    let mut group = c.benchmark_group("undo");
    for n in DEPTHS.into_iter().filter(|&n| n > WALKBACK) {
        group.bench_with_input(BenchmarkId::new("single_deep_jump", n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let view = build_history(&text, n);
                    let mut live = view.rope();
                    for _ in 0..WALKBACK {
                        live = earlier(&view, live).expect("history deeper than WALKBACK");
                    }
                    (view, live)
                },
                |(view, live)| black_box(earlier(&view, live).is_some()),
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

criterion_group!(undo, bench_cold_jump_back, bench_single_deep_jump);
criterion_main!(undo);
