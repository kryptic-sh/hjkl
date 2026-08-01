//! Viewport render hot-path benchmark.
//!
//! Measures `BufferView::render` for a full 120x50 frame over two line shapes:
//! - `short_lines`: short, chunk-contiguous rope lines.
//! - `long_lines`: 2000-char lines that likely straddle rope chunk boundaries.
//!
//! The `View` and `Viewport` are built once, outside the timing loop, so the
//! bench isolates render cost (not buffer construction). `Widget::render`
//! consumes `self`, but `BufferView` holds only references, so a fresh value is
//! rebuilt inside each iteration.

use criterion::{Criterion, criterion_group, criterion_main};

use hjkl_buffer::{View, Viewport, Wrap};
use hjkl_buffer_tui::BufferView;
use ratatui::buffer::Buffer as TermBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

fn no_styles(_id: u32) -> Style {
    Style::default()
}

/// No-wrap viewport covering the whole 120x50 frame at the document top.
fn viewport(width: u16, height: u16) -> Viewport {
    Viewport {
        top_row: 0,
        top_col: 0,
        width,
        height,
        wrap: Wrap::None,
        text_width: width,
        tab_width: 0,
    }
}

fn bench_render(c: &mut Criterion) {
    let area = Rect::new(0, 0, 120, 50);
    let vp = viewport(120, 50);
    let resolver: fn(u32) -> Style = no_styles;

    let short_text: String =
        std::iter::repeat_n("the quick brown fox jumps over\n", 5000).collect();
    let long_line = "x".repeat(2000);
    let long_text: String = std::iter::repeat_n(long_line.as_str(), 5000)
        .collect::<Vec<_>>()
        .join("\n");

    let short_view = View::from_str(&short_text);
    let long_view = View::from_str(&long_text);

    let mut group = c.benchmark_group("render");

    group.bench_function("short_lines", |b| {
        b.iter(|| {
            let view = BufferView {
                buffer: &short_view,
                viewport: &vp,
                selection: None,
                resolver: &resolver,
                cursor_line_bg: Style::default(),
                cursor_line_row: None,
                cursor_column: None,
                cursor_column_bg: Style::default(),
                selection_bg: Style::default(),
                cursor_style: Style::default(),
                gutter: None,
                search_bg: Style::default(),
                signs: &[],
                conceals: &[],
                spans: &[],
                search_pattern: None,
                search_ranges: None,
                non_text_style: Style::default(),
                show_eob: true,
                diag_overlays: &[],
                colorcolumn_cols: &[],
                colorcolumn_style: Style::default(),
                listchars: None,
                indent_guides_enabled: false,
                indent_guide_char: '│',
                indent_guide_shiftwidth: 4,
                indent_guide_fg: Color::Reset,
                indent_guide_active_fg: Color::Reset,
                indent_guide_active_col: None,
                fold_line_bg: Style::default(),
                folds_override: None,
                eol_hints: &[],
                blame_plan: None,
                diff_filler: None,
                background: Style::default(),
            };
            let mut term = TermBuffer::empty(area);
            view.render(area, &mut term);
            std::hint::black_box(&term);
        });
    });

    group.bench_function("long_lines", |b| {
        b.iter(|| {
            let view = BufferView {
                buffer: &long_view,
                viewport: &vp,
                selection: None,
                resolver: &resolver,
                cursor_line_bg: Style::default(),
                cursor_line_row: None,
                cursor_column: None,
                cursor_column_bg: Style::default(),
                selection_bg: Style::default(),
                cursor_style: Style::default(),
                gutter: None,
                search_bg: Style::default(),
                signs: &[],
                conceals: &[],
                spans: &[],
                search_pattern: None,
                search_ranges: None,
                non_text_style: Style::default(),
                show_eob: true,
                diag_overlays: &[],
                colorcolumn_cols: &[],
                colorcolumn_style: Style::default(),
                listchars: None,
                indent_guides_enabled: false,
                indent_guide_char: '│',
                indent_guide_shiftwidth: 4,
                indent_guide_fg: Color::Reset,
                indent_guide_active_fg: Color::Reset,
                indent_guide_active_col: None,
                fold_line_bg: Style::default(),
                folds_override: None,
                eol_hints: &[],
                blame_plan: None,
                diff_filler: None,
                background: Style::default(),
            };
            let mut term = TermBuffer::empty(area);
            view.render(area, &mut term);
            std::hint::black_box(&term);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_render);
criterion_main!(benches);
