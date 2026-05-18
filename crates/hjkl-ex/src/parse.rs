/// Split `input` into `(name, args)` where `name` is the leading
/// alphabetic-plus-optional-trailing-`!` prefix and `args` is the
/// remainder with leading whitespace stripped.
///
/// Examples:
/// - `"q"` → `("q", "")`
/// - `"q!"` → `("q!", "")`
/// - `"quit"` → `("quit", "")`
/// - `"q foo bar"` → `("q", "foo bar")`
/// - `"write /tmp/x"` → `("write", "/tmp/x")`
/// - `""` → `("", "")`
///
/// Range parsing is out of scope for Phase 1; the caller strips ranges before
/// handing `input` here.
pub(crate) fn split_name_args(input: &str) -> (&str, &str) {
    // Find end of alpha prefix
    let alpha_end = input
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .unwrap_or(input.len());

    // Allow an optional trailing '!' immediately after the alpha prefix
    let name_end = if input.as_bytes().get(alpha_end) == Some(&b'!') {
        alpha_end + 1
    } else {
        alpha_end
    };

    let name = &input[..name_end];
    let rest = input[name_end..].trim_start();
    (name, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_q() {
        assert_eq!(split_name_args("q"), ("q", ""));
    }

    #[test]
    fn q_bang() {
        assert_eq!(split_name_args("q!"), ("q!", ""));
    }

    #[test]
    fn quit() {
        assert_eq!(split_name_args("quit"), ("quit", ""));
    }

    #[test]
    fn q_with_args() {
        assert_eq!(split_name_args("q foo bar"), ("q", "foo bar"));
    }

    #[test]
    fn leading_whitespace_not_stripped_here() {
        // The caller (try_dispatch) trims leading whitespace before calling us.
        // split_name_args itself just handles alpha prefix + optional !
        assert_eq!(split_name_args("   q  "), ("", "q  "));
    }

    #[test]
    fn empty_input() {
        assert_eq!(split_name_args(""), ("", ""));
    }

    #[test]
    fn write_path() {
        assert_eq!(split_name_args("write /tmp/x"), ("write", "/tmp/x"));
    }
}
