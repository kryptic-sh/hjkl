# Security and Reliability Audit

**Date:** 2026-07-18 **Scope:** Rust workspace source review, with focus on
untrusted inputs, filesystem operations, process lifecycle, and error handling.

## Summary

| Severity | Count |
| -------- | ----: |
| High     |     1 |
| Medium   |     0 |
| Low      |     0 |

`cargo clippy --all-targets --all-features -- -D warnings` and
`cargo test --workspace --all-features` passed. Passing tests do not cover the
findings below.

## Findings

### High — nvim-api inbound size guard runs after reads instead of before them

**Location:** `apps/hjkl/src/nvim_api.rs:2394-2405,2421-2430`

`LimitedReader::read` fills the decoder-provided buffer before testing the
per-message limit. The MessagePack decoder receives this reader directly; a peer
can declare an oversized string, binary value, or container before the reader
records the limit breach, potentially inducing excessive allocation.

**Remediation:** Limit the supplied read slice to remaining budget before
calling `inner.read`, and error when no budget remains. Prefer decoder limits
that reject declared container/string sizes before allocation. Add oversized
`str32` and `bin32` regression tests.

## Notes

No source code changed during this audit. Findings are ordered by severity, then
by impact on data integrity, local resource exhaustion, and runtime recovery.
