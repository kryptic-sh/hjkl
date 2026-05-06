#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuerySanitizeReport {
    pub changed: bool,
    pub removed_lines: usize,
}

pub fn sanitize_highlights(src: &str) -> (String, QuerySanitizeReport) {
    let mut out = Vec::new();
    let mut removed = 0usize;

    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.contains("#set! @") {
            removed += 1;
            continue;
        }
        out.push(line);
    }

    let normalized_in = src.trim_end_matches('\n');
    let sanitized = out.join("\n");
    let changed = sanitized != normalized_in;
    (
        sanitized,
        QuerySanitizeReport {
            changed,
            removed_lines: removed,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_capture_set_directive_lines() {
        let src = "(tag_name) @tag\n(#set! @string.special.url url @string.special.url)\n";
        let (san, report) = sanitize_highlights(src);
        assert_eq!(san, "(tag_name) @tag");
        assert!(report.changed);
        assert_eq!(report.removed_lines, 1);
    }

    #[test]
    fn keeps_regular_set_directive_lines() {
        let src = "((attribute (quoted_attribute_value) @string)\n  (#set! priority 99))\n";
        let (san, report) = sanitize_highlights(src);
        assert_eq!(san, src.trim_end_matches('\n'));
        assert!(!report.changed);
        assert_eq!(report.removed_lines, 0);
    }
}
