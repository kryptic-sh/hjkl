//! Cost of building `textDocument/didOpen` / `didChange` notification params
//! for a large document.
//!
//! `json!` expands an interpolated expression to `serde_json::to_value(&expr)`,
//! which deep-copies the value even when it is already owned — so every
//! full-document LSP sync copied the whole buffer text one extra time. The
//! `*_json_macro` benches keep the original literals; the `*_moved` benches
//! use `hjkl_lsp::params`, which moves the text into the params object.
//!
//! Both variants take the text by value (a fresh `String` per iteration, built
//! in the setup closure and not timed) so they measure exactly the same work
//! apart from the copy.

use criterion::{Criterion, criterion_group, criterion_main};
use hjkl_lsp::event::TextChange;
use hjkl_lsp::params;
use serde_json::json;
use std::hint::black_box;

const URI: &str = "file:///tmp/ws/src/big.rs";

/// ~2 MiB of plausible source text.
fn document() -> String {
    let line = "    let value = compute(index, offset); // a representative source line\n";
    let mut text = String::with_capacity(2 * 1024 * 1024 + line.len());
    while text.len() < 2 * 1024 * 1024 {
        text.push_str(line);
    }
    text
}

fn changes(text: String) -> Vec<TextChange> {
    vec![
        TextChange {
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 0,
            text,
        },
        TextChange {
            start_line: 128,
            start_col: 4,
            end_line: 128,
            end_col: 12,
            text: "replacement".to_string(),
        },
    ]
}

fn bench_notify(c: &mut Criterion) {
    let text = document();
    let mut group = c.benchmark_group("lsp_notify_params");
    group.throughput(criterion::Throughput::Bytes(text.len() as u64));

    group.bench_function("did_open_json_macro", |b| {
        b.iter_batched(
            || (text.clone(), "rust".to_string()),
            |(text, language_id)| {
                black_box(json!({
                    "textDocument": {
                        "uri": URI,
                        "languageId": language_id,
                        "version": 1,
                        "text": text,
                    }
                }))
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.bench_function("did_open_moved", |b| {
        b.iter_batched(
            || (text.clone(), "rust".to_string()),
            |(text, language_id)| black_box(params::did_open(URI, language_id, 1, text)),
            criterion::BatchSize::LargeInput,
        );
    });

    group.bench_function("did_change_full_json_macro", |b| {
        b.iter_batched(
            || text.clone(),
            |text| {
                black_box(json!({
                    "textDocument": { "uri": URI, "version": 7 },
                    "contentChanges": [{ "text": text }],
                }))
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.bench_function("did_change_full_moved", |b| {
        b.iter_batched(
            || text.clone(),
            |text| black_box(params::did_change_full(URI, 7, text)),
            criterion::BatchSize::LargeInput,
        );
    });

    group.bench_function("did_change_incremental_json_macro", |b| {
        b.iter_batched(
            || changes(text.clone()),
            |changes| {
                let content_changes: Vec<serde_json::Value> = changes
                    .into_iter()
                    .map(|c| {
                        json!({
                            "range": {
                                "start": { "line": c.start_line, "character": c.start_col },
                                "end":   { "line": c.end_line,   "character": c.end_col },
                            },
                            "text": c.text,
                        })
                    })
                    .collect();
                black_box(json!({
                    "textDocument": { "uri": URI, "version": 7 },
                    "contentChanges": content_changes,
                }))
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.bench_function("did_change_incremental_moved", |b| {
        b.iter_batched(
            || changes(text.clone()),
            |changes| black_box(params::did_change_incremental(URI, 7, changes)),
            criterion::BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_notify);
criterion_main!(benches);
