# hjkl-icons

Filetype / directory → icon mapping for the [hjkl](https://hjkl.kryptic.sh)
editor stack.

Given a file extension or directory name, returns a single-character icon in one
of three modes:

- **`Nerd`** — [Nerd-Font](https://www.nerdfonts.com/) glyphs, per-filetype
  (requires a patched font).
- **`Unicode`** — geometric Unicode fallback (`▸ ▾ ·`) that renders in virtually
  every monospace font.
- **`Ascii`** — strict ASCII fallback (`> v`) that works literally everywhere.

```rust
use hjkl_icons::{IconMode, file_icon, dir_icon};

assert_eq!(file_icon(Some("rs"), IconMode::Unicode), '·');
assert_eq!(dir_icon(None, true, IconMode::Ascii), 'v');
```

> Note: there is no portable way to detect whether the terminal's font actually
> contains Nerd glyphs (terminals don't expose their font; a missing glyph and a
> real one both usually occupy one cell). Hosts should expose an explicit setting
> and may add a best-effort runtime probe for an `auto` mode — this crate stays a
> pure mapping and takes the resolved [`IconMode`] as input.
