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

Read in the tree on 2026-11-09:

- **Flags.** `hjkl_kitty::enable` pushes `DISAMBIGUATE_ESCAPE_CODES` (flag 1)
  only, unconditionally and without a probe (`apps/hjkl/src/main.rs`, terminal
  setup); `hjkl_kitty::disable` pops it on the normal exit path. gpur does the
  same. No other flag is ever pushed.
- **No panic, suspend or shell hand-over.** There is no panic hook restoring the
  terminal, `<C-z>` does not suspend, and a bare `:!cmd` runs without the
  terminal (`hjkl-ex` `shell.rs` documents this). So the flag stack is only
  wrong after a panic today — but each of those features, once added, must pop
  and re-push.
- **Decoding is crossterm 0.29's.** Keys reach hjkl as `crossterm::KeyEvent`.
  Its CSI-u parser (`parse_csi_u_encoded_key_code`) keeps only one key code:
  under flag 4 it swaps in the shifted key and clears Shift, dropping the
  unshifted and base-layout codes; flag 16 is not implemented
  (`REPORT_ASSOCIATED_TEXT` is commented out); modifyOtherKeys'
  `CSI 27 ; mod ; code ~` is not parsed at all.
- **Release / repeat.** `App::handle_key_event` drops `Release`;
  `hjkl_keymap_tui::from_crossterm` drops `Release` and passes `Repeat` as a
  press. Both are right for an editor and stay that way under flag 2.
- **Shift on characters is stripped blindly.** `from_crossterm` removes Shift
  from every `Char`, assuming the character already carries it. Under flag 1
  Alt+Shift+a arrives as `CSI 97;4u`, which crossterm reports as `'a'` + Shift +
  Alt, so a `<M-A>` mapping sees `<M-a>`. Confirmed by reading both parsers, not
  reproduced in a terminal. The app's own `chord_event_to_input`
  (`apps/hjkl/src/app/keymap.rs`) folds Shift only on ASCII letters — the
  backlog's §1.12 SHIFT-normalization item; this plan settles both.
- **The key model is narrow.** `hjkl_keymap::KeyCode` stops at `F(1..=12)` with
  no keypad, media, lock or modifier keys; `KeyModifiers` and the engine's
  `Input` carry only Shift, Ctrl and Alt — no Super, Hyper or Meta.
- **Key names are vim's.** `<C-x>`, `<S-x>`, `<A-x>` / `<M-x>` (`hjkl-keymap`
  `chord.rs`, `parse_modifier_tag`); the renderer emits `A-`.
- **Consumers.** tmxr uses `hjkl-picker-tui` (`handle_key` takes a crossterm
  `KeyEvent`); gpur uses `hjkl-kitty`. A change to the key types is a breaking
  change for both.

## Phases

1. **Fix Shift on modified characters.** Fold Shift into the character only when
   the character is the unshifted key of a letter (`'a'` + Shift → `'A'`), keep
   it where it is the only record of the shift (`'a'` + Shift + Alt → Alt +
   `'A'`), and use one rule in `from_crossterm` and `chord_event_to_input`
   (closes backlog §1.12 item 1). Pin `CSI 97;4u` → `<M-A>` and `CSI 104;6u` →
   `<C-S-h>` with crossterm-parsed fixtures. Small, ships first, independent of
   the rest.
2. **Terminal hand-over.** One guard owns raw mode, the alternate screen, mouse,
   paste, focus and the keyboard flags; a panic hook and every future
   suspend/shell path go through it. Use `CSI = flags ; 1 u` (set) inside the
   pushed level so a later flag change does not grow the stack.
3. **Own key decoder** — needs the owner's decision, see below. Decode `CSI … u`
   and the legacy/functional forms into a richer event: key, shifted key,
   base-layout key, all eight kitty modifiers, event kind, associated text.
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

- **How to decode keys (phase 3).** Options: (a) hjkl's own input reader — it
  must then also parse mouse, paste, focus and resize, which crossterm does for
  us today; (b) upstream flags 4 / 16 and modifyOtherKeys to crossterm and wait
  for a release; (c) a CSI-u-only pre-parser in front of crossterm's, which
  crossterm's reader does not allow without owning stdin. (a) is what tmxr is
  doing; sharing one decoder crate between hjkl and tmxr is the natural shape,
  and `hjkl-kitty` (already shared with gpur) is where it would live.
- **Breaking the key types.** Adding modifiers to `KeyModifiers` / `Input` and
  keys to `KeyCode` breaks `hjkl-picker-tui`'s and gpur's callers. Pre-1.0, so
  allowed; tmxr and gpur need the bump coordinated. tmxr asked (2026-11-09) to
  be told before the release that changes `hjkl-picker-tui::handle_key` ships:
  its session picker calls it.
- **Shared decoder — asked, not answered.** tmxr has put the decoder question to
  the owner (it needs one at its own plan step 3). tmxr's `<…>` key names now
  follow this plan's syntax, including `A-` = `M-`, and will add `D-` / `T-` as
  hjkl does.

## Key-name syntax (for tmxr's `<…>` form)

hjkl keeps vim / neovim notation and does not adopt kitty-style `alt+shift+h`
names inside `<…>`. The target after phases 1 and 6 (today `<M-S-a>` parses to
`'a'` + Shift + Alt, a different key from `<M-A>`):

- modifiers `C-` Ctrl, `S-` Shift, `A-` / `M-` Alt, and, added in phase 6,
  neovim's `D-` Super (Cmd) and `T-` Meta; order-insensitive, case-insensitive;
- a shifted letter is its uppercase character: `<M-A>` is Alt+Shift+a, and
  `<M-S-a>` parses to the same key; Ctrl+Shift+letter is `<C-S-h>`, distinct
  from `<C-h>`;
- key names as neovim spells them (`<CR>`, `<Esc>`, `<Tab>`, `<BS>`, `<F13>`,
  `<kEnter>`, …).
