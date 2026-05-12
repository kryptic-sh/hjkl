# hjkl-vim

Vim modal state types and grammar primitives for the hjkl editor stack. Provides
the `Mode` enum used as the mode discriminator in `hjkl-keymap`'s generic
`Keymap<A, M>`. Phase 2+ will land the vim FSM (transitions, operator-pending
resolution, count accumulation) here. For now the crate is pure plumbing: a
stable extraction point so the rest of the stack can depend on a versioned crate
rather than an in-tree enum.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

## Usage

```toml
[dependencies]
hjkl-vim = "0.1"
```

```rust
use hjkl_vim::Mode;

// Mode satisfies hjkl_keymap::Mode via the blanket impl for
// Copy + Eq + Hash + Debug types.
let mode = Mode::Normal;
```

## License

MIT — see [LICENSE](LICENSE).

## Contributing

See the [umbrella repo](https://github.com/kryptic-sh/hjkl) for contribution
guidelines and security policy.
