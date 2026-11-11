# 01 — Full kitty keyboard protocol

The owner asked (2026-11-09, relayed by the tmxr session) for full kitty
keyboard protocol support in hjkl, in step with tmxr's own plan
(`kryptic-sh/tmxr` `docs/plan/18-kitty-keyboard.md`). "Full" means:

- all five progressive-enhancement flags: 1 disambiguate, 2 event types (press /
  repeat / release), 4 alternate keys (shifted and base-layout), 8 all keys as
  escape codes, 16 associated text;
- push and pop at every terminal hand-over: start, exit, panic, suspend and
  resume;
- a fallback for terminals without it: xterm's modifyOtherKeys where the
  terminal has it, legacy VT input otherwise.

## Where it stands

Read in the tree on 2026-11-11, after phase 3:

- **Terminal I/O is ttyr 0.1's.** The app reads keys through one
  `ttyr::reader::Reader` (`main.rs` opens it, `App::run` polls it), draws
  through `ttyr::backend::TtyrBackend`, and turns every mode on through ttyr's
  restore guard (`ttyr::terminal::enable`), which also installs the panic hook.
  No hjkl crate depends on crossterm except `hjkl-kitty`, which hjkl no longer
  uses (see Decisions).
- **Flags.** `DISAMBIGUATE_ESCAPE_CODES` (flag 1) only, pushed unconditionally
  and without a probe through the guard (`Mode::KeyboardFlags`), with
  `reader.set_kitty_keyboard(true)`; popped by `ttyr::terminal::restore` on exit
  and by the panic hook. No other flag is pushed.
- **No suspend or shell hand-over.** `<C-z>` does not suspend, and a bare
  `:!cmd` runs without the terminal (`hjkl-ex` `shell.rs` documents this). Once
  either is added it goes through `ttyr::terminal::suspend_with`, which pops and
  re-pushes every mode.
- **One read boundary.** `typed_key` (`app/event_loop.rs`) turns each key the
  reader delivers into what the app matches on: ttyr's `normalize_legacy`
  (Ctrl+[ / Ctrl+I / Ctrl+M back to Esc / Tab / Enter), then a character key
  becomes `logical_char()` with `logical_modifiers()`. `ttyr_to_input`
  (`hjkl-engine-tui`) and `from_ttyr` (`hjkl-keymap-tui`) apply the same view,
  so library callers (tmxr's picker) can pass ttyr's raw event.
- **Release / repeat.** `App::handle_key_event` drops `Release`;
  `hjkl_keymap_tui::from_ttyr` drops `Release` and passes `Repeat` as a press.
  Both are right for an editor and stay that way under flag 2.
- **Shift on characters.** A letter typed with Shift reads as its capital from
  every encoding, Alt+Shift+a as `<M-A>` and Ctrl+Shift+h as `<C-H>` (pinned in
  `hjkl-keymap-tui`'s tests). Where Shift is the only record of the shift
  (Alt+Shift+1 under flag 1 reports `1`), `from_ttyr` still strips it, so it
  reads as `<M-1>`. The app's own `chord_event_to_input`
  (`apps/hjkl/src/app/keymap.rs`) folds Shift only on ASCII letters — backlog
  §1.12 item 1.
- **The key model is narrow.** `hjkl_keymap::KeyCode` stops at `F(1..=12)` with
  no keypad, media, lock or modifier keys; `KeyModifiers` and the engine's
  `Input` carry only Shift, Ctrl and Alt — no Super, Hyper or Meta (ttyr reports
  them; hjkl drops them).
- **Key names are vim's.** `<C-x>`, `<S-x>`, `<A-x>` / `<M-x>` (`hjkl-keymap`
  `chord.rs`, `parse_modifier_tag`); the renderer emits `A-`. `<C-S-h>` parses
  to `'h'` + Shift + Ctrl, which no key event matches, as `from_ttyr` strips
  Shift from characters; `<C-H>` is what Ctrl+Shift+h matches today.
- **Consumers.** tmxr calls `hjkl_picker_tui::handle_key`, which now takes
  `&ttyr::event::KeyEvent`; gpur uses `hjkl-kitty`.

## Phases

1. **Shift on modified digits and symbols.** Letters are settled by phase 3:
   ttyr's `logical_char()` reads Alt+Shift+a as `<M-A>` from flag 1 alone. What
   is left is a key whose shifted character the terminal did not report:
   Alt+Shift+1 under flag 1 arrives as `'1'` + Shift + Alt and reads as `<M-1>`.
   The choice:
   - keep Shift there (`<M-S-1>`), which needs `from_ttyr` to stop stripping it
     and `chord_event_to_input` to agree (closes backlog §1.12 item 1);
   - push flag 4 as well (phase 4), so the terminal reports `!` and it reads as
     `<M-!>`.

   Whether nvim gives `<M-!>` or `<M-S-1>` for Alt+Shift+1 in kitty was not
   checked, and is what should decide it.

2. **Terminal hand-over.** Mostly done by phase 3: ttyr's guard owns raw mode,
   the alternate screen, mouse, paste, focus and the keyboard flags, and its
   panic hook restores them. Left: a future suspend or shell path goes through
   `ttyr::terminal::suspend_with`, and a later flag change uses
   `CSI = flags ; 1 u` (ttyr's `SetKeyboardEnhancementFlags`) inside the pushed
   level so it does not grow the stack.
3. **Move from crossterm to `ttyr`.** Done 2026-11-11, on ttyr 0.1.0 from
   crates.io. Left over, in the backlog: Windows VT input (§1.14 #3), and
   whether to delete `hjkl-kitty` (Decisions below).
4. **Push all five flags** once phase 3 can read what they produce. Flag 8 turns
   Enter / Tab / Backspace and plain text into escape codes, so the insert path
   must take text from flag 16's associated text (or the key) rather than
   assuming a `Char` press is text; flag 2 releases stay dropped.
5. **modifyOtherKeys fallback.** Detect support without a blocking round-trip:
   send `CSI ? u` followed by DA1 at startup and read the replies through the
   normal event stream (the kitty reply arrives before DA1 only on a kitty
   terminal). Without kitty, enable `CSI > 4 ; 2 m` and decode
   `CSI 27 ; mod ; code ~`; reset it on every hand-over. Legacy otherwise.
6. **Key model and names.** Add Super and Meta (and Hyper if wanted) to
   `KeyModifiers` and `Input`, F13–F35, keypad and media keys. Names follow
   neovim's notation (see below). `<C-S-x>` becomes bindable distinct from
   `<C-x>` on a kitty terminal; on a legacy one it never arrives, exactly as in
   nvim.

## Decisions needed

- **Delete `hjkl-kitty`?** hjkl no longer uses it: ttyr's `normalize_legacy` and
  its guard's `Mode::KeyboardFlags` replace it. It stays in the workspace, still
  on crossterm, because gpur depends on it and it is published, so deleting it
  is the owner's call (settled 2026-11-09 with tmxr that it goes once gpur moves
  to ttyr).
- **Breaking the key types.** Adding modifiers to `KeyModifiers` / `Input` and
  keys to `KeyCode` (phase 6) breaks `hjkl-picker-tui`'s callers again. Pre-1.0,
  so allowed; tmxr asked (2026-11-09) to be told before a release that changes
  `hjkl-picker-tui::handle_key` ships.
- tmxr's `<…>` key names follow this plan's syntax, including `A-` = `M-`, and
  will add `D-` / `T-` as hjkl does.

## Key-name syntax (for tmxr's `<…>` form)

hjkl keeps vim / neovim notation and does not adopt kitty-style `alt+shift+h`
names inside `<…>`. The target after phases 1 and 6 (today `<M-S-a>` parses to
`'a'` + Shift + Alt, a different key from `<M-A>`, and `<C-S-h>` matches
nothing):

- modifiers `C-` Ctrl, `S-` Shift, `A-` / `M-` Alt, and, added in phase 6,
  neovim's `D-` Super (Cmd) and `T-` Meta; order-insensitive, case-insensitive;
- a shifted letter is its uppercase character: `<M-A>` is Alt+Shift+a, and
  `<M-S-a>` parses to the same key; Ctrl+Shift+letter is `<C-S-h>`, distinct
  from `<C-h>`;
- key names as neovim spells them (`<CR>`, `<Esc>`, `<Tab>`, `<BS>`, `<F13>`,
  `<kEnter>`, …).
