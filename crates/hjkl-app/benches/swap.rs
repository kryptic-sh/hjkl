//! Full swap-file write cost vs document size and undo-tree depth — evidence
//! for the deferred "swap tail append-log" item of issue #302.
//!
//! Today every swap write is a FULL rewrite: `write_swap_full`
//! (`src/swap.rs`) postcard-encodes the header, postcard-encodes the whole
//! [`SwapUndo`] section (which embeds `SerTree.base` — a complete copy of the
//! root document text — plus one `SerNode` per history node), streams the
//! entire rope body, `fsync`s, and renames. `tick_idle_swap_write`
//! (`apps/hjkl/src/app/event_loop.rs`) fires that once per dirty slot after
//! every `updatetime` idle gap, so a normal editing session with natural
//! pauses re-serializes the entire undo tree over and over. The proposal is an
//! append-only per-group delta log instead of a full body rewrite; these
//! numbers say whether the serialized tree is actually the part that hurts.
//!
//! **fsync is INSIDE the measurement.** `write_swap_full` calls
//! `File::sync_all` on the temp file (and best-effort `sync_all` on the parent
//! directory after the rename), and there is no way to reach the real write
//! path without it. To keep that cost comparable across runs instead of
//! hostage to disk state, the benched files live on **tmpfs**: `/dev/shm` when
//! it exists and is writable, otherwise `std::env::temp_dir()` (which may be a
//! real disk — absolute numbers then shift, the shape does not). Each bench
//! gets its own `tempfile::TempDir` under that base, removed on drop.
//!
//! Because fsync-to-tmpfs is still an I/O syscall pair, `serialize_undo`
//! benches the pure-CPU half on its own: exactly the `postcard::to_stdvec` call
//! `write_swap_full` makes to produce the undo-section bytes, nothing else. So
//! depth scaling is visible without any I/O in the way.
//!
//! Both benches build their rope and undo history in `iter_batched` SETUP —
//! and in fact hoist it further, into a per-matrix-cell fixture built before
//! `iter_batched` is even called, since `write_swap_full` and `to_stdvec` only
//! take shared references and cannot perturb it. The timed closure contains
//! nothing but the write / serialize call.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use hjkl_app::swap::{SwapHeader, SwapUndo, write_swap_full};
use hjkl_buffer::{Edit, MarkSnapshot, Position, UndoEntry, View};
use ropey::Rope;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Line width for the synthetic document — realistic prose-ish.
const WIDTH: usize = 60;

/// Document sizes: a small file that fits on a couple of screens, and a large
/// one where the streamed body is ~1.2 MB.
const SIZES: [(&str, usize); 2] = [("small_200l", 200), ("large_20000l", 20_000)];

/// Undo depths (committed nodes beyond the root). `0` means *no undo section
/// at all* for `write_full` (`undo == None`, the `write_swap` content-only
/// shape) — that is the floor the append-log could aim at. For
/// `serialize_undo` a depth of `0` is the fresh single-node tree, since "cost
/// of serializing nothing" is not a data point.
const DEPTHS: [usize; 3] = [0, 64, 1024];

fn base_text(lines: usize) -> String {
    let line: String = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(WIDTH)
        .collect();
    let mut text = String::with_capacity((WIDTH + 1) * lines);
    for i in 0..lines {
        text.push_str(&line);
        if i + 1 < lines {
            text.push('\n');
        }
    }
    text
}

/// Build a `View` with a LINEAR undo history of `n` committed nodes.
///
/// Same construction as `hjkl-buffer/benches/undo.rs::build_history`, which is
/// the established pattern and mirrors what `Editor::push_undo_at` +
/// `mutate_edit` do: push an `UndoEntry` holding the PRE-edit rope/cursor/marks,
/// then mutate the buffer. Timestamps are deterministic (`UNIX_EPOCH + i s`) so
/// no wall-clock jitter leaks into the serialized bytes.
fn build_history(base: &str, n: usize, lines: usize) -> View {
    let mut view = View::from_str(base);
    for i in 0..n {
        let cursor = view.cursor();
        view.push_undo_entry(UndoEntry {
            rope: view.rope(),
            cursor: (cursor.row, cursor.col),
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(i as u64),
            marks: MarkSnapshot::default(),
        });
        let row = i % lines;
        view.apply_edit(Edit::InsertStr {
            at: Position::new(row, 0),
            text: format!("e{i} "),
        });
    }
    view
}

/// The live rope plus the swap undo section for a `(lines, depth)` cell.
///
/// `undo` is `None` at depth 0 so the matrix includes the no-undo-section
/// baseline that `write_swap` produces.
fn fixture(lines: usize, depth: usize) -> (Rope, Option<SwapUndo>) {
    let view = build_history(&base_text(lines), depth, lines);
    let rope = view.rope();
    let undo = (depth > 0).then(|| {
        let (tree, current_seq) = view.undo_to_serializable();
        SwapUndo { tree, current_seq }
    });
    (rope, undo)
}

/// A real `SwapUndo` for every depth — depth 0 included, as the fresh
/// single-node tree. Used by `serialize_undo`, which measures serialization
/// cost and so has no use for "absent".
fn undo_fixture(lines: usize, depth: usize) -> SwapUndo {
    let view = build_history(&base_text(lines), depth, lines);
    let (tree, current_seq) = view.undo_to_serializable();
    SwapUndo { tree, current_seq }
}

fn header(path: &str) -> SwapHeader {
    SwapHeader {
        magic: SwapHeader::MAGIC,
        version: SwapHeader::VERSION,
        canonical_path: path.to_string(),
        file_mtime_unix_ms: 1_700_000_000_000,
        write_time_unix_ms: 1_700_000_001_000,
        cursor: (3, 7),
        writer_pid: std::process::id(),
    }
}

/// tmpfs base for the benched swap files: `/dev/shm` when writable, else the
/// platform temp dir. Probed at runtime — a `tempdir_in` that succeeds is the
/// only writability proof worth having.
fn tmpfs_base() -> PathBuf {
    let shm = PathBuf::from("/dev/shm");
    if tempfile::Builder::new()
        .prefix("hjkl-bench-probe")
        .tempdir_in(&shm)
        .is_ok()
    {
        shm
    } else {
        std::env::temp_dir()
    }
}

/// `write_swap_full` end to end — header + undo section + body + fsync +
/// rename. The number the append-log would reduce.
fn bench_write_full(c: &mut Criterion) {
    let dir = tempfile::Builder::new()
        .prefix("hjkl-swap-write")
        .tempdir_in(tmpfs_base())
        .expect("create bench temp dir");
    let path = dir.path().join("bench.swp");
    let hdr = header("/bench/doc.rs");

    let mut group = c.benchmark_group("swap");
    for (label, lines) in SIZES {
        for depth in DEPTHS {
            // Untimed: rope + full undo history for this cell, built once.
            // `write_swap_full` takes only `&` args, so one fixture serves
            // every iteration without drift.
            let (rope, undo) = fixture(lines, depth);
            let id = BenchmarkId::new("write_full", format!("{label}/d{depth}"));
            group.bench_function(id, |b| {
                b.iter_batched(
                    || (rope.clone(), undo.as_ref()),
                    |(rope, undo)| {
                        write_swap_full(&path, &hdr, &rope, undo).expect("swap write");
                        black_box(())
                    },
                    BatchSize::SmallInput,
                )
            });
        }
    }
    group.finish();
    // TempDir removes the directory (and any leftover .tmp) on drop.
    drop(dir);
}

/// CPU-only undo-section serialization: exactly the
/// `postcard::to_stdvec(&SwapUndo)` call `write_swap_full` makes for the undo
/// section, with no file, no fsync, no body streaming. Isolates how the
/// serialized-tree cost grows with history depth — and shows how much of it is
/// the `SerTree.base` document copy rather than the nodes.
fn bench_serialize_undo(c: &mut Criterion) {
    let mut group = c.benchmark_group("swap");
    for (label, lines) in SIZES {
        for depth in DEPTHS {
            // Untimed: the whole tree, built once for this cell.
            let undo = undo_fixture(lines, depth);
            let id = BenchmarkId::new("serialize_undo", format!("{label}/d{depth}"));
            group.bench_function(id, |b| {
                b.iter_batched(
                    || &undo,
                    |undo| {
                        let bytes = postcard::to_stdvec(undo).expect("postcard undo");
                        black_box(bytes)
                    },
                    BatchSize::SmallInput,
                )
            });
        }
    }
    group.finish();
}

criterion_group!(swap, bench_write_full, bench_serialize_undo);
criterion_main!(swap);
