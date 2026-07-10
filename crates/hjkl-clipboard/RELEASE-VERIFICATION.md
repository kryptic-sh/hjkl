# Manual release verification matrix

Before tagging a `hjkl-clipboard` release, run the checklist below on a real
machine for each environment. See `DESIGN-0.4.0.md` for backend architecture
details.

Mark each cell as `pass`, `fail`, or `skip` (with reason). At least one tester
must sign off on each row before the tag is pushed. Record results as a comment
on the release PR or in the release notes.

## Legend

| Symbol | Meaning                              |
| ------ | ------------------------------------ |
| ST     | `set` text (`MimeType::Text`)        |
| GT     | `get` text                           |
| SP     | `set` PNG (`MimeType::Png`)          |
| GP     | `get` PNG                            |
| SH     | `set` HTML (`MimeType::Html`)        |
| CL     | `clear`                              |
| AV     | `available` returns correct MIME set |

## Matrix

| Environment                                  | ST  | GT  | SP  | GP  | SH  | CL  | AV  | Notes                               |
| -------------------------------------------- | --- | --- | --- | --- | --- | --- | --- | ----------------------------------- |
| Linux Wayland — sway                         |     |     |     |     |     |     |     |                                     |
| Linux Wayland — KDE Plasma                   |     |     |     |     |     |     |     |                                     |
| Linux Wayland — GNOME (OSC 52 fallback path) | —   | —   | —   | —   | —   |     |     | write-only; SP/GP/SH unsupported    |
| Linux X11 — with klipper / GPaste            |     |     |     |     |     |     |     | persistence via SAVE_TARGETS        |
| Linux X11 — no clipboard manager             |     |     |     |     |     |     |     | SAVE_TARGETS should fail gracefully |
| macOS desktop session                        |     |     |     |     |     |     |     |                                     |
| Windows 10 / 11                              |     |     |     |     |     |     |     |                                     |
| OSC 52 in TTY (kitty / WezTerm)              | —   | —   | —   | —   | —   |     |     | write-only; no read-back            |

## Per-cell procedure

For each `set` cell:

1. Call `cb.set(Selection::Clipboard, MimeType::*, payload)`.
2. Paste into a native app (e.g., gedit, Terminal, Preview, Notepad). Verify
   content is correct.

For each `get` cell:

1. Copy data into the clipboard from a native app.
2. Call `cb.get(Selection::Clipboard, MimeType::*)`. Verify returned bytes.

For `clear`:

1. Set something. Call `cb.clear(Selection::Clipboard)`. Attempt to paste —
   should be empty or produce a "nothing to paste" response.

For `available`:

1. Set text + HTML. Call `cb.available(Selection::Clipboard)`. Verify returned
   `Vec<MimeType>` contains at least `[Text, Html]`.

## GNOME OSC 52 path

On GNOME, `Clipboard::new()` falls back to the OSC 52 backend because Mutter
does not expose `ext_data_control_v1`. Test in a terminal emulator that supports
OSC 52 write (kitty, WezTerm, iTerm2). Verify `set` delivers text to the
terminal's clipboard and that `get` returns `UnsupportedMime`.

## PRIMARY selection (Linux only)

Run the ST / GT cells for `Selection::Primary` on both Wayland and X11. Paste
with middle-click to verify. On Wayland the `zwp_primary_selection_v1` protocol
must be supported by the compositor.
