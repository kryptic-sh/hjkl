# hjkl-xdg

XDG Base Directory resolution — env-first, `$HOME`-fallback, uniform across
platforms.

Honors `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` when set to
an absolute path. Falls back to `~/.config` / `~/.local/share` / `~/.cache` on
every platform — including macOS and Windows. Deliberately does not use
platform-native paths (`~/Library/Application Support`, `%APPDATA%`) so that
hjkl-style CLI tools produce an identical layout everywhere.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

## Usage

```rust
use hjkl_xdg::{config_dir, data_dir, cache_dir};

let cfg = config_dir("myapp")?;  // ~/.config/myapp
let data = data_dir("myapp")?;   // ~/.local/share/myapp
let cache = cache_dir("myapp")?; // ~/.cache/myapp
```
