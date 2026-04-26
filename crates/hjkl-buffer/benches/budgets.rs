//! Performance budget benches per `MIGRATION.md` "Performance Budgets".
//!
//! Each bench targets a budget defined in the plan. The harness runs
//! per-PR and posts deltas vs `main` to commit status; regressions >5%
//! against the recorded baseline fail CI.
//!
//! Budgets covered here:
//!
//! - Insert char at cursor (1 MB buffer): <5 μs
//! - Search-next on 10k-line buffer: <1 ms
//! - Cold load 10 MB file into rope: <50 ms

// 0.0.37: search FSM moved off `Buffer` per step 3 of
// `DESIGN_33_METHOD_CLASSIFICATION.md`. The bench drives the regex
// `find_iter` pass directly against the buffer's text (the
// `BufferView` renderer + the engine's `search_*` free functions both
// compose this primitive). No deprecated accessors involved.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use hjkl_buffer::{Buffer, Edit, Position};
use regex::Regex;

fn make_buffer(line_count: usize, line_width: usize) -> Buffer {
    let line: String = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(line_width)
        .collect();
    let mut text = String::with_capacity((line_width + 1) * line_count);
    for i in 0..line_count {
        text.push_str(&line);
        if i + 1 < line_count {
            text.push('\n');
        }
    }
    Buffer::from_str(&text)
}

fn build_text(line_count: usize, line_width: usize) -> String {
    let line: String = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(line_width)
        .collect();
    let mut text = String::with_capacity((line_width + 1) * line_count);
    for i in 0..line_count {
        text.push_str(&line);
        if i + 1 < line_count {
            text.push('\n');
        }
    }
    text
}

fn bench_insert_char(c: &mut Criterion) {
    // ~1 MB buffer: 12 800 lines × 80 chars = 1.024 MB.
    let text = build_text(12_800, 80);
    c.bench_function("insert_char_1MB_buffer", |b| {
        b.iter_batched(
            || Buffer::from_str(&text),
            |mut buf| {
                let pos = Position::new(6_400, 40);
                let edit = Edit::InsertChar { at: pos, ch: 'x' };
                buf.apply_edit(black_box(edit));
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_search_next(c: &mut Criterion) {
    let buf = make_buffer(10_000, 80);
    let re = Regex::new("lazy").expect("regex");
    c.bench_function("search_next_10k_lines", |b| {
        b.iter(|| {
            // Mirror the engine's `search_forward` per-row scan path:
            // walk rows from cursor onward and stop at the first row
            // with a match. The regex compile lives outside the loop;
            // the work measured is the per-row find_iter scan.
            for row_idx in 0..buf.row_count() {
                let line = buf.line(row_idx).unwrap_or("");
                if black_box(re.find(line)).is_some() {
                    break;
                }
            }
        })
    });
    let _ = Position::new(0, 0); // silence unused if other benches drift
}

fn bench_cold_load(c: &mut Criterion) {
    // ~10 MB: 128k lines × 80 chars.
    let line: String = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(80)
        .collect();
    let mut text = String::with_capacity(81 * 128_000);
    for i in 0..128_000 {
        text.push_str(&line);
        if i + 1 < 128_000 {
            text.push('\n');
        }
    }
    c.bench_function("cold_load_10MB", |b| {
        b.iter(|| Buffer::from_str(black_box(&text)))
    });
}

criterion_group!(
    budgets,
    bench_insert_char,
    bench_search_next,
    bench_cold_load
);
criterion_main!(budgets);
