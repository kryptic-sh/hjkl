//! Syntax-span install hot-path benchmark.
//!
//! The syntax worker hands the engine a fresh span table on (almost) every
//! keystroke, so `install_syntax_spans` / `patch_syntax_spans_range` sit
//! directly on the per-keystroke path. Both funnel through
//! `translate_row_spans`, which clamps each span to its row's byte length —
//! the row-length lookup is the part that historically cost a full,
//! mutex-locked `String` clone of the line.
//!
//! Shapes measured, all against a 10k-row buffer (the size where the cost
//! became visible as j/k lag):
//!
//! - `install_full_viewport_populated`: full-file install where only a
//!   viewport-sized window (50 rows) carries spans — what a sync
//!   `query_viewport` produces. Cost should track populated rows, not file
//!   size.
//! - `install_full_all_rows_populated`: worst case — every row carries
//!   spans (a full-file parse delivered in one shot).
//! - `patch_viewport_range`: the incremental path, patching just the
//!   visible 50 rows.
//!
//! The buffer and the span table are built once, outside the timing loop;
//! the span input is handed over as a borrowing iterator chain so the bench
//! measures translation, not `Vec` construction.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use hjkl_buffer::View;
use hjkl_engine::Editor;
use hjkl_engine::types::{Attrs, Color, DefaultHost, Options, Style};

const ROWS: usize = 10_000;
const VIEWPORT: usize = 50;
/// Spans per populated row — roughly what tree-sitter emits for a dense
/// line of code.
const SPANS_PER_ROW: usize = 12;

/// A 10k-row buffer of realistic code-shaped lines (~70 bytes each).
fn big_buffer() -> View {
    let text = (0..ROWS)
        .map(|i| format!("    let value_{i} = compute(alpha, beta, gamma) + offset_{i};"))
        .collect::<Vec<_>>()
        .join("\n");
    View::from_str(&text)
}

/// A handful of distinct styles, cycled per span — mirrors the small,
/// bounded set of token kinds a grammar produces (and keeps `intern_style`'s
/// linear scan at a realistic depth).
fn palette() -> Vec<Style> {
    (0..8u8)
        .map(|i| Style {
            fg: Some(Color(20 * i, 100, 200 - 10 * i)),
            bg: None,
            attrs: if i % 3 == 0 {
                Attrs::BOLD
            } else {
                Attrs::empty()
            },
        })
        .collect()
}

/// Span table with `populated` rows carrying `SPANS_PER_ROW` spans each,
/// starting at row `start`; every other row of the 10k is empty.
fn span_table(start: usize, populated: usize) -> Vec<Vec<(usize, usize, Style)>> {
    let palette = palette();
    let mut table: Vec<Vec<(usize, usize, Style)>> = vec![Vec::new(); ROWS];
    let row_spans: Vec<(usize, usize, Style)> = (0..SPANS_PER_ROW)
        .map(|s| {
            let lo = s * 5;
            (lo, lo + 4, palette[s % palette.len()])
        })
        .collect();
    let end = (start + populated).min(ROWS);
    for slot in &mut table[start..end] {
        slot.clone_from(&row_spans);
    }
    table
}

fn editor() -> Editor<View, DefaultHost> {
    Editor::new(big_buffer(), DefaultHost::new(), Options::default())
}

fn bench_install(c: &mut Criterion) {
    let mut group = c.benchmark_group("syntax_span_install");

    let table = span_table(0, VIEWPORT);
    let mut ed = editor();
    group.bench_function("install_full_viewport_populated", |b| {
        b.iter(|| {
            ed.install_syntax_spans(table.iter().map(|row| row.iter().copied()));
            black_box(ed.buffer_spans().len())
        });
    });

    let table_all = span_table(0, ROWS);
    let mut ed = editor();
    group.bench_function("install_full_all_rows_populated", |b| {
        b.iter(|| {
            ed.install_syntax_spans(table_all.iter().map(|row| row.iter().copied()));
            black_box(ed.buffer_spans().len())
        });
    });

    group.finish();
}

fn bench_patch(c: &mut Criterion) {
    let mut group = c.benchmark_group("syntax_span_patch");

    // Viewport parked in the middle of the file, as it is after any real
    // amount of scrolling.
    let top = ROWS / 2;
    let table = span_table(top, VIEWPORT);
    let window: Vec<Vec<(usize, usize, Style)>> = table[top..top + VIEWPORT].to_vec();
    let mut ed = editor();
    // Prime the row-sized span vectors so the bench doesn't measure the
    // one-off resize.
    ed.patch_syntax_spans_range(
        top..top + VIEWPORT,
        window.iter().map(|row| row.iter().copied()),
    );
    group.bench_function("patch_viewport_range", |b| {
        b.iter(|| {
            ed.patch_syntax_spans_range(
                top..top + VIEWPORT,
                window.iter().map(|row| row.iter().copied()),
            );
            black_box(ed.buffer_spans().len())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_install, bench_patch);
criterion_main!(benches);
