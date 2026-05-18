# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.3.0] - 2026-05-16

### Added

- `Keymap::add_if(mode, chord, action, desc, predicate)` — registers a
  predicate-gated binding. The predicate is
  `Arc<dyn Fn() -> bool + Send + Sync>`; when it returns `false` at resolve
  time, `lookup()` skips the binding and the key falls through as `Unbound`
  while the trie node stays present so longer chord extensions remain reachable.
- `Binding<A>.condition: Option<Arc<dyn Fn() -> bool + Send + Sync>>` — optional
  gating predicate stored on every binding (always `None` for bindings
  registered via the existing `add` / `add_chord` methods).
- `Binding::is_active() -> bool` — evaluates the condition; returns `true` when
  no predicate is set (always-active) or when the predicate returns `true`.
- `Ambiguous` + false-predicate resolution: when the complete arm of an
  ambiguous chord has a false predicate, resolution yields `Pending` rather than
  `Ambiguous`, letting the chord extend naturally.
- Doc guidance on `add_if` vs always-bound action variants: reach for `add_if`
  when silent fall-through is desired (e.g. git hunk actions outside a repo);
  use an always-bound action variant when user feedback (toast) is appropriate.
  Intended consumers: #39 scripting layer, #113 extension API, #115 git hunk
  actions.

## [0.2.0] - 2026-05-12

### Changed (breaking)

- `Keymap` is now generic over both the action type `A` and a mode discriminator
  `M`: `Keymap<A, M: Mode>`. Every public method that took the old concrete
  `Mode` enum now takes `M`. This unblocks editor-FSM crates (`hjkl-vim`,
  `hjkl-helix`, `hjkl-emacs`, `hjkl-vscode`) defining their own modal vocabulary
  instead of being forced into vim's.

### Removed (breaking)

- The concrete `Mode` enum (`Normal` / `Insert` / `Visual` / `OpPending` /
  `CommandLine`). Consumers must now define their own mode type — any
  `Copy + Eq + Hash + Debug` type satisfies the new `Mode` trait via a blanket
  impl.

### Added

- `Mode` trait with blanket impl: `trait Mode: Copy + Eq + Hash + Debug`. No
  manual `impl Mode for T` is needed.

### Migration

Consumers replace:

```rust
use hjkl_keymap::Mode;
let km: Keymap<MyAction> = Keymap::new(' ');
km.feed(Mode::Normal, ev, now);
```

with their own mode enum:

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
enum MyMode { Normal, Insert /* … */ }

let km: Keymap<MyAction, MyMode> = Keymap::new(' ');
km.feed(MyMode::Normal, ev, now);
```

The blanket impl `impl<T: Copy + Eq + Hash + Debug> Mode for T {}` means any
suitable enum (or other type) works out of the box.

## [0.1.4] - 2026-05-12

### Fixed

- `Keymap::timeout_resolve` now leaves the pending buffer in place when the
  buffer is a pure prefix (no terminal binding at the current depth but deeper
  bindings exist). Previously it drained the buffer in this case, causing
  which-key popups to disappear the instant the chord-timeout fired on a leader
  prefix. Three documented outcomes: terminal match → `Match` (drain), pure
  prefix → `Unbound(empty)` (no drain), dead-end → `Unbound` with drained
  events.

## [0.1.3] - 2026-05-12

### Added

- `Keymap::pop(mode) -> Option<KeyEvent>` — removes and returns the last key
  from the pending chord buffer for the given mode. Returns `None` when the
  buffer is already empty. Used by which-key callers to implement
  Backspace-as-navigate-up: the user backs out of a chord prefix one key at a
  time without resetting the whole buffer.

## [0.1.2] - 2026-05-12

### Added

- `Keymap::children_all(mode, prefix) -> Vec<(KeyEvent, Option<Binding<A>>)>` —
  returns both terminal bindings AND prefix-only submenu nodes for the immediate
  children of a chord prefix. `None` binding indicates a prefix-only entry (a
  submenu with no terminal action of its own). Complements existing `children()`
  which returns terminals only. Designed for which-key popups that need to show
  submenu indicators.

## [0.1.1] - 2026-05-12

### Added

- `<gt>` chord-notation escape for literal `>` — cosmetic symmetry with the
  existing `<lt>` escape. Bare `>` continues to parse as `Char('>')`.

## [0.1.0] - 2026-05-10

### Added

#### Key types

- `KeyEvent` — backend-agnostic key press: `code: KeyCode` +
  `modifiers: KeyModifiers`.
- `KeyCode` — logical key variants: `Char(char)`, `Enter`, `Esc`, `Tab`,
  `Backspace`, `Delete`, `Insert`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`,
  `PageUp`, `PageDown`, `F(u8)` (F1–F12).
- `KeyModifiers` — `bitflags`-backed modifier set: `NONE`, `SHIFT`, `CTRL`,
  `ALT`; combinable with `|`.
- `KeyEvent::char(c)` and `KeyEvent::ctrl(c)` convenience constructors.

#### Chord parser / serializer

- `Chord` — ordered sequence of `KeyEvent`s forming a multi-key binding.
- `Chord::parse(s, leader)` — parses vim-style notation:
  - `<leader>` expands to the supplied leader char.
  - Modifier combos: `<C-x>`, `<S-x>`, `<A-x>`, `<C-S-Tab>`, `<C-A-x>`,
    `<S-A-x>` (M- alias accepted for A-).
  - Named specials: `<Esc>`, `<CR>`, `<Tab>`, `<BS>`, `<Del>`, `<Insert>`,
    `<Up>`, `<Down>`, `<Left>`, `<Right>`, `<Home>`, `<End>`, `<PageUp>`,
    `<PageDown>`, `<F1>`–`<F12>`, `<Space>`, `<lt>`.
  - Bare characters map to `Char(c)` with no modifiers.
- `Chord::to_notation(leader)` — round-trips back to vim notation.
- `ChordParseError` — `UnclosedAngle`, `UnknownSpecial`, `BadModifierTarget`
  variants.

#### Keymap dispatch

- `Mode` — `Normal`, `Insert`, `Visual`, `OpPending`, `CommandLine`.
- `Keymap<A>` — modal keymap storing per-mode tries; generic over action type.
  - `Keymap::new(leader)` — construct with leader char.
  - `set_leader`, `set_timeout`, `leader`, `timeout_duration` — configuration.
  - `add(mode, chord_str, action, desc)` — parse + register a binding.
  - `add_chord(mode, chord, binding)` — register a pre-parsed binding.
  - `remove(mode, chord_str)` — unregister a binding; returns whether removed.
  - `feed(mode, ev, now) -> KeyResolve<A>` — stateful per-key dispatch with
    per-mode pending buffer.
  - `timeout_resolve(mode) -> KeyResolve<A>` — force-resolve on timeout expiry.
  - `pending(mode) -> &[KeyEvent]` — snapshot of in-progress chord buffer.
  - `reset(mode)` — clear pending buffer (e.g. on mode switch).
  - `children(mode, prefix) -> Vec<(KeyEvent, Binding<A>)>` — which-key
    enumeration of completions reachable from a prefix chord.
- `KeyResolve<A>` — `Pending`, `Match(Binding<A>)`, `Ambiguous`,
  `Unbound(Vec<KeyEvent>)`.
- `Binding<A>` — `action: A`, `desc: String`, `recursive: bool` (reserved).
- `KeymapError` — `Parse(ChordParseError)`, `EmptyChord`.

[Unreleased]: https://github.com/kryptic-sh/hjkl-keymap/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/kryptic-sh/hjkl-keymap/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/kryptic-sh/hjkl-keymap/releases/tag/v0.2.0
[0.1.4]: https://github.com/kryptic-sh/hjkl-keymap/releases/tag/v0.1.4
[0.1.3]: https://github.com/kryptic-sh/hjkl-keymap/releases/tag/v0.1.3
[0.1.2]: https://github.com/kryptic-sh/hjkl-keymap/releases/tag/v0.1.2
[0.1.1]: https://github.com/kryptic-sh/hjkl-keymap/releases/tag/v0.1.1
[0.1.0]: https://github.com/kryptic-sh/hjkl-keymap/releases/tag/v0.1.0
