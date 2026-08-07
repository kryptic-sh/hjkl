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

/// Cap on how much of a scanned line the modeline parser reads, in chars.
/// A modeline is a short comment (`# vim: set ts=2`); nothing realistic sits
/// past this, and the cap bounds the worst case (a single giant line) no
/// matter how big the file is. Mirrors the seam's bound in `hjkl-lang`
/// (`detect::MODELINE_LINE_CAP`), which reads `ft=` from the same lines.
const MODELINE_LINE_CAP: usize = 500;

/// First [`MODELINE_LINE_CAP`] chars of `line`, cut at a char boundary.
fn cap_modeline_line(line: &str) -> &str {
    match line.char_indices().nth(MODELINE_LINE_CAP) {
        Some((byte_idx, _)) => &line[..byte_idx],
        None => line,
    }
}

/// Scan `content` for vim modelines and return the parsed option overrides.
///
/// `scan_depth` lines from the top AND bottom of the file are checked
/// (matching vim's `modelines` default of 5); the middle is never examined —
/// `lines().rev()` is double-ended, so the bottom pass starts from the end
/// via a backward newline search and a huge file costs O(depth) lines, not
/// O(lines). Each line is read for at most [`MODELINE_LINE_CAP`] chars.
///
/// Later lines win: entries are emitted top-first, then the bottom lines in
/// line order, and the caller applies them left to right (vim applies every
/// modeline's options in line order, later ones overriding earlier ones).
/// A file with fewer than 2×depth lines has its top and bottom ranges
/// overlap, so it is scanned whole, in line order — exactly the lines the
/// two ranges would cover, once each.
pub fn parse_modelines(content: &str, scan_depth: usize) -> Vec<(String, OptionValue)> {
    let mut out = Vec::new();
    // Count up to 2×depth lines to see whether the top and bottom ranges
    // overlap. Bounded by 2×depth even on a huge file.
    let total = content.lines().take(2 * scan_depth).count();
    if total < 2 * scan_depth {
        // Small file: the ranges collide, so scan every line once, in order.
        for line in content.lines() {
            parse_line(line, &mut out);
        }
        return out;
    }
    // Top `scan_depth` lines, then the bottom `scan_depth` in line order
    // (`rev()` yields them bottom-up) — no overlap, each line once.
    for line in content.lines().take(scan_depth) {
        parse_line(line, &mut out);
    }
    let mut bottom: Vec<&str> = content.lines().rev().take(scan_depth).collect();
    bottom.reverse();
    for line in bottom {
        parse_line(line, &mut out);
    }
    out
}

/// Try to extract modeline options from a single line, appending to `out`.
/// Reads at most [`MODELINE_LINE_CAP`] chars of the line.
fn parse_line(line: &str, out: &mut Vec<(String, OptionValue)>) {
    let line = cap_modeline_line(line);
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

    // Tokenise: split on whitespace. Vim terminates the modeline at the first
    // `:` after the options — everything past that colon is ordinary comment
    // text, not more options. So the token that *carries* the trailing colon
    // is the last one we look at; we parse it, then stop. Breaking only on an
    // empty token (a bare `:`) let the trailing comment of
    // `/* vim: set ts=2: list of pending items */` keep parsing, and words
    // like `list` / `wrap` / `number` / `expandtab` are perfectly ordinary
    // English that would silently flip real options.
    for token in body.split_whitespace() {
        let (token, terminates) = match token.strip_suffix(':') {
            // e.g. "et:" → "et", and nothing after this token is a modeline.
            Some(stripped) => (stripped, true),
            None => (token, false),
        };
        // An empty token (the bare `:` case) is rejected by `parse_token`.
        if let Some(entry) = parse_token(token) {
            out.push(entry);
        }
        if terminates {
            break;
        }
    }
}

/// Find the earliest `vim:` / `ex:` / `vi:` marker in `line`.
/// Returns `(byte_offset_of_marker, &str_after_colon)`.
fn find_marker(line: &str) -> Option<(usize, &str)> {
    // Pick the marker with the smallest byte offset, not the first entry of
    // the list that happens to occur anywhere in the line: vim parses from the
    // *first* marker on the line, so `# vi: ts=2 vim: sw=4` is a `vi:` modeline
    // whose options are `ts=2` (list-order matching would have started at
    // `vim:` and thrown away the `vi:` options).
    ["vim:", "ex:", "vi:"]
        .iter()
        .filter_map(|marker| {
            line.find(marker)
                .map(|pos| (pos, &line[pos + marker.len()..]))
        })
        .min_by_key(|(pos, _)| *pos)
}

/// Parse a single `key=value`, `key`, or `nokey` token into `(name, value)`.
///
/// Alias resolution happens via a scratch `Options` and `set_by_name` — if
/// the token name is unknown to `set_by_name` the option is silently dropped.
fn parse_token(token: &str) -> Option<(String, OptionValue)> {
    // Validate via set_by_name on a scratch Options — this doubles as
    // alias resolution (the canonical name is the one used in set_by_name's
    // match arms, but we expose the user-supplied alias unchanged since
    // set_by_name already accepts aliases).
    let accepts = |name: &str, val: &OptionValue| {
        let mut probe = Options::default();
        probe.set_by_name(name, val.clone()).is_ok()
    };

    // key=value — try numeric first, then string.
    if let Some((k, v)) = token.split_once('=') {
        let value = if let Ok(n) = v.parse::<i64>() {
            OptionValue::Int(n)
        } else {
            OptionValue::String(v.to_owned())
        };
        return accepts(k, &value).then(|| (k.to_owned(), value));
    }

    // Bare name → Bool(true). The whole token has to be tried as a name FIRST:
    // stripping a leading "no" before validating mangles the real options that
    // simply start with those two letters, and `number` is one of them — it
    // became `umber`, `set_by_name` rejected it, and `# vim: number` was
    // dropped without a trace. `nonumber` kept working (it strips to `number`),
    // which is why the bug stayed hidden.
    let on = OptionValue::Bool(true);
    if accepts(token, &on) {
        return Some((token.to_owned(), on));
    }

    // Not an option under its own name — now read a leading "no" as negation
    // (`noet` → et=false, `nonumber` → number=false).
    let bare = token.strip_prefix("no")?;
    let off = OptionValue::Bool(false);
    accepts(bare, &off).then(|| (bare.to_owned(), off))
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

    // ── parse_modeline_later_line_wins ────────────────────────────────────────

    #[test]
    fn parse_modeline_later_line_wins() {
        // vim applies every modeline in line order, later ones overriding
        // earlier ones — a bottom ts=4 must beat a top ts=2.
        let mut lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        lines[0] = "# vim: ts=2:".to_string();
        lines[99] = "# vim: ts=4:".to_string();
        let content = lines.join("\n");
        let opts = opts_with_modeline(&content);
        assert_eq!(opts.tabstop, 4, "bottom modeline must override the top one");
    }

    #[test]
    fn parse_modeline_small_file_scans_every_line_once() {
        // A file smaller than 2×depth has overlapping ranges; every line is
        // scanned exactly once (later lines still win), and a single line is
        // not double-applied.
        let entries = parse_modelines("# vim: ts=2 sw=2 et:\n", 5);
        assert_eq!(
            entries.len(),
            3,
            "a single-line file must yield exactly 3 entries, not duplicates"
        );
    }

    // ── parse_modeline_beyond_line_cap ────────────────────────────────────────

    #[test]
    fn parse_modeline_beyond_line_cap_is_ignored() {
        // Per-line scan is capped at MODELINE_LINE_CAP chars; an option
        // sitting past the cap is invisible (deliberate bound). 4 is the
        // engine default, untouched by the cap'd-out modeline.
        let mut line = "x".repeat(600);
        line.push_str(" # vim: ts=7:");
        let content = format!("{line}\n");
        let opts = opts_with_modeline(&content);
        assert_eq!(
            opts.tabstop, 4,
            "modeline beyond the line cap must not apply"
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

    // ── parse_modeline_bare_no_prefixed_option ────────────────────────────────

    #[test]
    fn parse_modeline_bare_no_prefixed_option() {
        // `number` is a real option that happens to start with "no". Stripping
        // the negation prefix before validating the name turned it into
        // `umber`, which `set_by_name` rejects — so the option vanished.
        let entries = parse_modelines("# vim: number\n", 5);
        assert!(
            entries
                .iter()
                .any(|(n, v)| n == "number" && *v == OptionValue::Bool(true)),
            "`# vim: number` must parse as (\"number\", Bool(true)), got {entries:?}"
        );

        // And it must actually reach Options. `number` defaults to true, so
        // start from false to prove the modeline is what flipped it.
        let mut opts = Options {
            number: false,
            ..Options::default()
        };
        overlay_modeline_for_content(&mut opts, "# vim: number\n", 5);
        assert!(opts.number, "`# vim: number` must enable 'number'");
    }

    // ── parse_modeline_no_prefix_negation_still_works ─────────────────────────

    #[test]
    fn parse_modeline_no_prefix_negation_still_works() {
        // The working half of the bug above: `nonumber` is not itself an
        // option name, so it must still fall through to negation.
        let entries = parse_modelines("# vim: nonumber\n", 5);
        assert!(
            entries
                .iter()
                .any(|(n, v)| n == "number" && *v == OptionValue::Bool(false)),
            "`nonumber` must parse as (\"number\", Bool(false)), got {entries:?}"
        );

        let opts = opts_with_modeline("# vim: nonumber\n");
        assert!(!opts.number, "`# vim: nonumber` must disable 'number'");
    }

    // ── parse_modeline_stops_at_terminating_colon ─────────────────────────────

    #[test]
    fn parse_modeline_stops_at_terminating_colon() {
        // Vim ends the modeline at the colon after the options; the rest of
        // the line is comment prose. `list` is both a real option and an
        // ordinary English word, so a trailing comment used to flip it on.
        let content = "/* vim: set ts=2: list of stuff */\n";
        let entries = parse_modelines(content, 5);
        assert!(
            entries
                .iter()
                .any(|(n, v)| n == "ts" && *v == OptionValue::Int(2)),
            "options before the terminating colon must still apply, got {entries:?}"
        );
        assert!(
            !entries.iter().any(|(n, _)| n == "list"),
            "words after the terminating colon are comment text, not options: {entries:?}"
        );

        let opts = opts_with_modeline(content);
        assert_eq!(opts.tabstop, 2, "ts=2 must apply");
        assert!(!opts.list, "the trailing comment must not enable 'list'");
    }

    // ── parse_modeline_uses_earliest_marker ───────────────────────────────────

    #[test]
    fn parse_modeline_uses_earliest_marker() {
        // Two markers on one line: vim parses from the first one, so this is a
        // `vi:` modeline (`ts=2`) whose body happens to mention `vim:`.
        // Matching markers in list order started at `vim:` and lost `ts=2`.
        let entries = parse_modelines("# vi: ts=2 vim: sw=4\n", 5);
        assert!(
            entries
                .iter()
                .any(|(n, v)| n == "ts" && *v == OptionValue::Int(2)),
            "options must come from the earliest marker (`vi:`), got {entries:?}"
        );
        assert!(
            !entries.iter().any(|(n, _)| n == "sw"),
            "`vim:` here terminates the `vi:` modeline; sw=4 is comment text: {entries:?}"
        );
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
