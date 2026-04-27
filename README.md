# hjkl-clipboard

Unified clipboard sink for the hjkl editor stack.

Tries `arboard` first (native X11/Wayland/macOS/Windows). Falls back to OSC 52
when over SSH or when arboard is unavailable. Wraps OSC 52 in a tmux DCS
passthrough when running inside tmux.
