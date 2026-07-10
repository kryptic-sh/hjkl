# Contributing to `hjkl-clipboard`

Thanks for considering a contribution. `hjkl-clipboard` is pre-1.0 and the
public API is still in motion — please open an issue before starting any
non-trivial PR so the design can be sanity-checked early.

## Development setup

```bash
git clone git@github.com:kryptic-sh/hjkl-clipboard.git
cd hjkl-clipboard
rustup toolchain install stable
cargo test --all-features
```

## MSRV policy

`rust-version` in `Cargo.toml` tracks current stable Rust. Floor, not ceiling —
bumps land freely when new features are useful. Any bump must be logged in
`CHANGELOG.md` under the version that introduces it.

## Pull requests

- Branch from `main`. One logical change per PR.
- Commits: [Conventional Commits](https://www.conventionalcommits.org/) format.
  `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `build`.
  Scope optional.
- Run before pushing:
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --all-features`
- New public API needs rustdoc and (where applicable) a `///` example.

## Releases

Patch bumps follow the BCTP flow (Bump → Commit → Tag → Push). Lockstep with
sibling `hjkl-*` crates is preferred when changes touch shared types.

To **yank** a broken release:

```bash
cargo yank --version X.Y.Z
```

Yank ≠ delete: consumers pinned to `=X.Y.Z` still resolve. Document the reason
in `CHANGELOG.md` under a `### Yanked` heading for that version.

## Pre-1.0 stability

Pre-1.0, breaking changes may land on minor bumps per Cargo's SemVer rules for
`0.x`. Inter-crate sibling deps in this family use caret (`"0.3"`) and ship with
patch-bump cadence; consumers can pin tighter if needed.

## Reporting bugs / requesting features

Open a GitHub issue. For security issues, see `SECURITY.md` — do not file public
issues.

## Code of Conduct

This project follows the
[Contributor Covenant](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).

## Manual release verification matrix

Before tagging a release, run the checklist below on a real machine for each
environment. See `DESIGN-0.4.0.md` for backend architecture details.

Mark each cell as `pass`, `fail`, or `skip` (with reason). At least one tester
must sign off on each row before the tag is pushed. Record results as a comment
on the release PR or in the release notes.

### Legend

| Symbol | Meaning                              |
| ------ | ------------------------------------ |
| ST     | `set` text (`MimeType::Text`)        |
| GT     | `get` text                           |
| SP     | `set` PNG (`MimeType::Png`)          |
| GP     | `get` PNG                            |
| SH     | `set` HTML (`MimeType::Html`)        |
| CL     | `clear`                              |
| AV     | `available` returns correct MIME set |

### Matrix

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

### Per-cell procedure

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

### GNOME OSC 52 path

On GNOME, `Clipboard::new()` falls back to the OSC 52 backend because Mutter
does not expose `ext_data_control_v1`. Test in a terminal emulator that supports
OSC 52 write (kitty, WezTerm, iTerm2). Verify `set` delivers text to the
terminal's clipboard and that `get` returns `UnsupportedMime`.

### PRIMARY selection (Linux only)

Run the ST / GT cells for `Selection::Primary` on both Wayland and X11. Paste
with middle-click to verify. On Wayland the `zwp_primary_selection_v1` protocol
must be supported by the compositor.
