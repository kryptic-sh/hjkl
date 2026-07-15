//! Vim modeline parser — scans the first/last N lines of a buffer for
//! `vim:` / `ex:` / `vi:` markers and returns per-buffer option overrides.
//!
//! Two syntax forms are accepted:
//!
//! ```text
//! # vim: set ts=2 sw=2 et:
//! # vim: ts=2 sw=2 et
//! # ex: set ts=2:
//! ```
//!
//! Result is a `Vec<(name, value)>` that the caller applies via
//! `Options::set_by_name`. Only options known to `set_by_name` are emitted;
//! unknown tokens are dropped silently.

use hjkl_engine::types::{OptionValue, Options};

// ── Parser ────────────────────────────────────────────────────────────────────

/// Scan `content` for vim modelines and return the parsed option overrides.
///
/// `scan_depth` lines from the top AND bottom of the file are checked
/// (matching vim's `modelines` default of 5). Duplicates are not collapsed —
/// later entries win when the caller applies them left-to-right.
pub fn parse_modelines(content: &str, scan_depth: usize) -> Vec<(String, OptionValue)> {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    // Build the set of line indices to scan — first N and last N, deduplicated.
    // We collect into a Vec and process in order so callers get a stable result.
    let top_end = scan_depth.min(total);
    let bot_start = total.saturating_sub(scan_depth);

    let mut indices: Vec<usize> = (0..top_end).collect();
    for i in bot_start..total {
        if i >= top_end {
            indices.push(i);
        }
    }

    let mut out = Vec::new();
    for idx in indices {
        parse_line(lines[idx], &mut out);
    }
    out
}

/// Try to extract modeline options from a single line, appending to `out`.
fn parse_line(line: &str, out: &mut Vec<(String, OptionValue)>) {
    // Find a `vim:` / `ex:` / `vi:` marker.  The character immediately before
    // the marker must be start-of-line, whitespace, or a non-alphanumeric
    // character — so `xvim:` is rejected but `// vim:` and `#vim:` are accepted.
    let Some((marker_start, rest)) = find_marker(line) else {
        return;
    };

    // Validate word-boundary: char before marker must be absent (start-of-line)
    // or non-alphanumeric.
    if marker_start > 0 {
        let before = line[..marker_start].chars().next_back().unwrap_or(' ');
        if before.is_alphanumeric() {
            return;
        }
    }

    // Strip optional leading whitespace after the marker.
    let rest = rest.trim_start();

    // Strip optional `set ` keyword.
    let body = if let Some(after_set) = rest
        .strip_prefix("set ")
        .or_else(|| rest.strip_prefix("set\t"))
    {
        after_set
    } else {
        rest
    };

    // Tokenise: split on whitespace, stop at a bare `:` token (terminator).
    for token in body.split_whitespace() {
        // Trim a trailing colon from the last real token (e.g. "et:" → "et").
        let token = token.strip_suffix(':').unwrap_or(token);
        if token.is_empty() {
            break;
        }
        if let Some(entry) = parse_token(token) {
            out.push(entry);
        }
    }
}

/// Find the earliest `vim:` / `ex:` / `vi:` marker in `line`.
/// Returns `(byte_offset_of_marker, &str_after_colon)`.
fn find_marker(line: &str) -> Option<(usize, &str)> {
    for marker in &["vim:", "ex:", "vi:"] {
        if let Some(pos) = line.find(marker) {
            let after = &line[pos + marker.len()..];
            return Some((pos, after));
        }
    }
    None
}

/// Parse a single `key=value`, `key`, or `nokey` token into `(name, value)`.
///
/// Alias resolution happens via a scratch `Options` and `set_by_name` — if
/// the token name is unknown to `set_by_name` the option is silently dropped.
fn parse_token(token: &str) -> Option<(String, OptionValue)> {
    let (name_raw, val) = if let Some((k, v)) = token.split_once('=') {
        // key=value — try numeric first, then string.
        let value = if let Ok(n) = v.parse::<i64>() {
            OptionValue::Int(n)
        } else {
            OptionValue::String(v.to_owned())
        };
        (k, value)
    } else if let Some(bare) = token.strip_prefix("no") {
        // nokey → Bool(false)
        (bare, OptionValue::Bool(false))
    } else {
        // bare key → Bool(true) for booleans, skipped for non-booleans
        (token, OptionValue::Bool(true))
    };

    // Validate via set_by_name on a scratch Options — this doubles as
    // alias resolution (the canonical name is the one used in set_by_name's
    // match arms, but we expose the user-supplied alias unchanged since
    // set_by_name already accepts aliases).
    let mut probe = Options::default();
    if probe.set_by_name(name_raw, val.clone()).is_ok() {
        Some((name_raw.to_owned(), val))
    } else {
        None
    }
}

// ── Overlay ───────────────────────────────────────────────────────────────────

/// Apply modeline overrides from `content` on top of `opts`.
///
/// Scans `scan_depth` lines from each end, then calls `Options::set_by_name`
/// for each recognised option. Unknown options are logged at `debug` level.
pub fn overlay_modeline_for_content(opts: &mut Options, content: &str, scan_depth: usize) {
    for (name, val) in parse_modelines(content, scan_depth) {
        if let Err(e) = opts.set_by_name(&name, val) {
            // Options unknown to set_by_name were already filtered by
            // parse_token; this branch only fires for type errors
            // (e.g. a string value for a bool option).
            tracing::debug!(option = %name, reason = %e, "modeline: skipping option");
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::types::Options;

    fn opts_with_modeline(content: &str) -> Options {
        let mut opts = Options::default();
        overlay_modeline_for_content(&mut opts, content, 5);
        opts
    }

    // ── parse_modeline_basic_form ─────────────────────────────────────────────

    #[test]
    fn parse_modeline_basic_form() {
        let entries = parse_modelines("# vim: ts=2 sw=2 et:\n", 5);
        assert_eq!(entries.len(), 3, "expected 3 options: ts, sw, et");
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"ts"), "should contain ts");
        assert!(names.contains(&"sw"), "should contain sw");
        assert!(names.contains(&"et"), "should contain et");
    }

    // ── parse_modeline_set_form ───────────────────────────────────────────────

    #[test]
    fn parse_modeline_set_form() {
        let entries = parse_modelines("# vim: set ts=2 sw=2 et:\n", 5);
        assert_eq!(entries.len(), 3, "`set` form should yield same 3 options");
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"ts"));
        assert!(names.contains(&"sw"));
        assert!(names.contains(&"et"));
    }

    // ── parse_modeline_noet_form ──────────────────────────────────────────────

    #[test]
    fn parse_modeline_noet_form() {
        let entries = parse_modelines("# vim: noet ts=8:\n", 5);
        // Should have expandtab=false and tabstop=8
        let et = entries
            .iter()
            .find(|(n, _)| n == "et" || n == "expandtab" || n == "noet");
        // noet strips to "et" with Bool(false)
        let noet = entries
            .iter()
            .find(|(n, v)| n == "et" && *v == OptionValue::Bool(false));
        assert!(noet.is_none() || et.is_some(), "noet should be present");

        // Check the actual structure: noet parses as name="et" val=Bool(false)
        let found_noet = entries
            .iter()
            .find(|(n, v)| n == "et" && *v == OptionValue::Bool(false));
        assert!(
            found_noet.is_some(),
            "noet should parse to (\"et\", Bool(false))"
        );

        let found_ts = entries
            .iter()
            .find(|(n, v)| n == "ts" && *v == OptionValue::Int(8));
        assert!(found_ts.is_some(), "ts=8 should be parsed");
    }

    // ── parse_modeline_last_lines ─────────────────────────────────────────────

    #[test]
    fn parse_modeline_last_lines() {
        // 10-line file; modeline only at the last line; scan_depth=3.
        let mut lines: Vec<String> = (0..9).map(|i| format!("line {i}")).collect();
        lines.push("# vim: ts=3 sw=3:".to_string());
        let content = lines.join("\n");

        let entries = parse_modelines(&content, 3);
        assert!(
            !entries.is_empty(),
            "modeline in last line should be picked up"
        );
        assert!(
            entries
                .iter()
                .any(|(n, v)| n == "ts" && *v == OptionValue::Int(3))
        );
    }

    // ── parse_modeline_outside_scan_depth ────────────────────────────────────

    #[test]
    fn parse_modeline_outside_scan_depth() {
        // Modeline on line index 5 (the 6th line) with scan_depth=5.
        // scan checks lines 0..5 (top) and len-5..len (bottom).
        // For a 12-line file: top=0..5, bot=7..12 → line 5 is NOT covered.
        let mut lines: Vec<String> = (0..12).map(|i| format!("line {i}")).collect();
        lines[5] = "# vim: ts=99 sw=99:".to_string();
        let content = lines.join("\n");

        let entries = parse_modelines(&content, 5);
        assert!(
            !entries
                .iter()
                .any(|(n, v)| n == "ts" && *v == OptionValue::Int(99)),
            "modeline at line 5 in a 12-line file with depth=5 should NOT be picked up"
        );
    }

    // ── parse_modeline_unknown_option_ignored ─────────────────────────────────

    #[test]
    fn parse_modeline_unknown_option_ignored() {
        let entries = parse_modelines("# vim: ts=2 bogus=42:\n", 5);
        // Only ts=2 is emitted; bogus=42 is silently dropped.
        assert!(
            entries
                .iter()
                .any(|(n, v)| n == "ts" && *v == OptionValue::Int(2))
        );
        assert!(!entries.iter().any(|(n, _)| n == "bogus"));
    }

    // ── parse_modeline_rejects_makeprg (security, CVE-2019-12735 class) ───────

    #[test]
    fn parse_modeline_rejects_makeprg() {
        // vim's `:set makeprg=` / `errorformat` from a modeline is the
        // classic arbitrary-command-on-`:make` CVE. `hjkl` has no
        // `makeprg`/`errorformat` fields on `Options` at all, so
        // `set_by_name` rejects them the same way it rejects any unknown
        // option — this pins that a modeline can never smuggle either in,
        // even though `ts=2` right next to it is still honored normally.
        let entries = parse_modelines("# vim: ts=2 makeprg=pwned errorformat=fmt:\n", 5);
        assert!(
            entries
                .iter()
                .any(|(n, v)| n == "ts" && *v == OptionValue::Int(2)),
            "an unrelated, legitimate option on the same line must still work"
        );
        assert!(
            !entries.iter().any(|(n, _)| n == "makeprg"),
            "makeprg must never be emitted from a modeline"
        );
        assert!(
            !entries.iter().any(|(n, _)| n == "errorformat"),
            "errorformat must never be emitted from a modeline"
        );
    }

    // ── parse_modeline_marker_must_be_word_boundary ───────────────────────────

    #[test]
    fn parse_modeline_marker_must_be_word_boundary() {
        // "xvim:" — 'x' is alphanumeric, so NOT a valid modeline marker.
        let entries = parse_modelines("xvim: ts=2:\n", 5);
        assert!(
            entries.is_empty(),
            "xvim: should be rejected (alphanumeric before marker)"
        );
    }

    // ── parse_modeline_alias_resolution ──────────────────────────────────────

    #[test]
    fn parse_modeline_alias_resolution() {
        // Verify that short aliases all resolve through set_by_name.
        let line = "# vim: ts=2 sw=3 tw=80 sts=2 et noic noscs:\n";
        let entries = parse_modelines(line, 5);

        let has = |name: &str, val: &OptionValue| -> bool {
            entries.iter().any(|(n, v)| n == name && v == val)
        };

        assert!(has("ts", &OptionValue::Int(2)), "ts alias");
        assert!(has("sw", &OptionValue::Int(3)), "sw alias");
        assert!(has("tw", &OptionValue::Int(80)), "tw alias");
        assert!(has("sts", &OptionValue::Int(2)), "sts alias");
        assert!(has("et", &OptionValue::Bool(true)), "et alias");
        assert!(has("ic", &OptionValue::Bool(false)), "noic alias");
        assert!(has("scs", &OptionValue::Bool(false)), "noscs alias");
    }

    // ── overlay_applies_to_options ────────────────────────────────────────────

    #[test]
    fn overlay_applies_to_options() {
        let content = "# vim: ts=3 sw=3 noet:\n";
        let opts = opts_with_modeline(content);
        assert_eq!(opts.tabstop, 3, "modeline ts=3 should set tabstop=3");
        assert_eq!(opts.shiftwidth, 3, "modeline sw=3 should set shiftwidth=3");
        assert!(!opts.expandtab, "modeline noet should set expandtab=false");
    }

    // ── overlay_layered_after_editorconfig ────────────────────────────────────

    #[test]
    fn overlay_layered_after_editorconfig() {
        // Simulate editorconfig setting ts=4, then modeline overrides to ts=2.
        let mut opts = Options {
            tabstop: 4,
            ..Options::default()
        };
        overlay_modeline_for_content(&mut opts, "# vim: ts=2:\n", 5);
        assert_eq!(
            opts.tabstop, 2,
            "modeline ts=2 must win over editorconfig ts=4"
        );
    }
}
