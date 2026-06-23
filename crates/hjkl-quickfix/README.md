# hjkl-quickfix

Renderer-agnostic quickfix / location-list data model for [hjkl](https://hjkl.kryptic.sh) editors.

A [`QfList`] is an ordered list of [`QfEntry`] locations (file + line + col +
kind + message) with a cursor pointer and vim-style navigation
(`next` / `prev` / `first` / `last` / `nth`). The same type backs both the
global quickfix list and per-window location lists; population (`:grep`,
`:make`, LSP references) and rendering live in the host.
