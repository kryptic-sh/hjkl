# hjkl-completion-tui

Ratatui adapter for `hjkl-app` completion.

Paints an LSP/word-completion popup into a ratatui `Frame` given a `Completion`
model from `hjkl-app`.

## Usage

```rust
use hjkl_completion_tui::{CompletionTheme, popup};
use hjkl_theme::Color;
use ratatui::layout::Rect;

// Build a theme from your app's color palette:
let theme = CompletionTheme {
    border: Color::rgb(0x61, 0xaf, 0xef),
    selected_bg: Color::rgb(0x3e, 0x44, 0x51),
    normal_fg: Color::rgb(0xe5, 0xe9, 0xf0),
    detail_fg: Color::rgb(0x5c, 0x63, 0x70),
};

// Compute the cursor cell in absolute screen coordinates:
let anchor = Rect { x: abs_col, y: abs_row, width: 1, height: 1 };

// Render into the frame (inside your draw closure):
// popup(frame, &completion, &theme, anchor, buf_area);
```

## Overflow handling

When the popup would extend past the bottom of `viewport`, it automatically
flips above the cursor anchor.

## License

MIT
