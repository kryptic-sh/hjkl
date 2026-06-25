# hjkl-kitty

Kitty keyboard protocol support for crossterm-based terminal UIs.

Provides `enable`/`disable` helpers that push/pop
`DISAMBIGUATE_ESCAPE_CODES` and a `normalize_legacy` helper that maps the
disambiguated Ctrl+[/i/m bytes back to Esc/Tab/Enter for vim-modal
disciplines.

Reusable across kryptic-sh TUIs: **hjkl**, **sqeel**, and others that use
crossterm as their terminal backend.
