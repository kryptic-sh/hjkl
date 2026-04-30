//! Windows `CF_HDROP` / `DROPFILES` structure builder and parser.
//!
//! Used for `text/uri-list` on Windows. Handles UNC path mapping:
//! `\\server\share\foo` ↔ `file://server/share/foo`.
//!
//! **Phase 3c stub** — implementation pending.
