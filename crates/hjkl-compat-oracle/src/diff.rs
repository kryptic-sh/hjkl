//! Compare hjkl and neovim outcomes for each corpus case.

use crate::{Corpus, OracleCase, hjkl_driver, nvim_driver, nvim_driver::nvim_available};

/// Per-case oracle result.
pub struct CaseResult {
    pub name: String,
    pub status: CaseStatus,
}

/// Outcome of running a single oracle case.
pub enum CaseStatus {
    /// Both engines agreed on every compared field.
    Pass,
    /// The engines (or the corpus expectation) disagreed on a field. Only the
    /// first mismatch is recorded.
    Mismatch {
        field: &'static str,
        expected: String,
        got_hjkl: String,
        got_nvim: String,
    },
    /// The hjkl engine returned an error.
    HjklError(String),
    /// The nvim driver returned an error.
    NvimError(String),
    /// The case was not run (e.g. nvim not installed).
    Skipped(String),
}

impl std::fmt::Debug for CaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => write!(f, "Pass"),
            Self::Mismatch {
                field,
                expected,
                got_hjkl,
                got_nvim,
            } => f
                .debug_struct("Mismatch")
                .field("field", field)
                .field("expected", expected)
                .field("got_hjkl", got_hjkl)
                .field("got_nvim", got_nvim)
                .finish(),
            Self::HjklError(e) => write!(f, "HjklError({e})"),
            Self::NvimError(e) => write!(f, "NvimError({e})"),
            Self::Skipped(r) => write!(f, "Skipped({r})"),
        }
    }
}

impl std::fmt::Debug for CaseResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaseResult")
            .field("name", &self.name)
            .field("status", &self.status)
            .finish()
    }
}

/// Drive both engines for every case in `corpus` and return the results.
pub async fn run_oracle(corpus: &Corpus) -> Vec<CaseResult> {
    let nvim_ok = nvim_available();
    let mut results = Vec::with_capacity(corpus.cases.len());

    for case in &corpus.cases {
        results.push(run_single(case, nvim_ok).await);
    }

    results
}

async fn run_single(case: &OracleCase, nvim_ok: bool) -> CaseResult {
    let name = case.name.clone();

    // Run hjkl driver first — both the full diff and the no-nvim fallback
    // need its outcome.
    let hjkl = match hjkl_driver::run_case(case) {
        Ok(o) => o,
        Err(e) => {
            return CaseResult {
                name,
                status: CaseStatus::HjklError(e.to_string()),
            };
        }
    };

    if !nvim_ok {
        // No neovim to diff against — pin hjkl to the corpus's authored
        // expectations (nvim's measured outcome at authoring time) so the
        // corpus still guards something on machines without nvim, including
        // CI lanes that do not install it. Every case carries at least an
        // `expected_buffer`, so none needs to skip.
        return check_against_authored(case, name, hjkl);
    }

    // Run nvim driver.
    let nvim = match nvim_driver::run_case(case).await {
        Ok(o) => o,
        Err(e) => {
            return CaseResult {
                name,
                status: CaseStatus::NvimError(e.to_string()),
            };
        }
    };

    // Sanity-check nvim against corpus expected_buffer.
    if nvim.buffer != case.expected_buffer {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_buffer (corpus author error?)",
                expected: case.expected_buffer.clone(),
                got_hjkl: hjkl.buffer.clone(),
                got_nvim: nvim.buffer,
            },
        };
    }

    // Sanity-check nvim against the corpus's other pinned expectations, same
    // role the `expected_buffer` guard above plays: without these, a case
    // could author any cursor, mode or register at all and still pass as long
    // as the two engines happened to agree with each other.
    if let Some(expected) = case.expected_cursor
        && nvim.cursor != expected
    {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_cursor (corpus author error?)",
                expected: format!("{expected:?}"),
                got_hjkl: format!("{:?}", hjkl.cursor),
                got_nvim: format!("{:?}", nvim.cursor),
            },
        };
    }

    if let Some(expected) = case.expected_mode.as_deref()
        && nvim.mode != expected
    {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_mode (corpus author error?)",
                expected: expected.to_string(),
                got_hjkl: hjkl.mode,
                got_nvim: nvim.mode,
            },
        };
    }

    if let Some((_, expected)) = case.expected_register.as_ref()
        && nvim.pinned_register.as_deref() != Some(expected.as_str())
    {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_register (corpus author error?)",
                expected: expected.clone(),
                got_hjkl: format!("{:?}", hjkl.pinned_register),
                got_nvim: format!("{:?}", nvim.pinned_register),
            },
        };
    }

    // Compare buffer.
    if hjkl.buffer != nvim.buffer {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "buffer",
                expected: nvim.buffer.clone(),
                got_hjkl: hjkl.buffer,
                got_nvim: nvim.buffer,
            },
        };
    }

    // Compare cursor. Both sides are char-indexed — `nvim_driver` converts
    // the byte column nvim reports — so a case may hold non-ASCII text.
    if hjkl.cursor != nvim.cursor {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "cursor",
                expected: format!("{:?}", nvim.cursor),
                got_hjkl: format!("{:?}", hjkl.cursor),
                got_nvim: format!("{:?}", nvim.cursor),
            },
        };
    }

    // Compare mode.
    if hjkl.mode != nvim.mode {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "mode",
                expected: nvim.mode.clone(),
                got_hjkl: hjkl.mode,
                got_nvim: nvim.mode,
            },
        };
    }

    // Compare default register.
    if hjkl.default_register != nvim.default_register {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "default_register",
                expected: nvim.default_register.clone(),
                got_hjkl: hjkl.default_register,
                got_nvim: nvim.default_register,
            },
        };
    }

    // Compare the pinned register. Only a case that names one has anything
    // here; the unnamed register is already covered above.
    if hjkl.pinned_register != nvim.pinned_register {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "pinned_register",
                expected: format!("{:?}", nvim.pinned_register),
                got_hjkl: format!("{:?}", hjkl.pinned_register),
                got_nvim: format!("{:?}", nvim.pinned_register),
            },
        };
    }

    CaseResult {
        name,
        status: CaseStatus::Pass,
    }
}

/// Fallback used when nvim is absent: compare hjkl's outcome against the
/// corpus's authored `expected_*` fields (nvim's measured outcome at
/// authoring time), so the expectations pin hjkl on their own instead of
/// every case being skipped wholesale. `expected_buffer` is required on every
/// case; the cursor / mode / register checks apply only when authored.
fn check_against_authored(
    case: &OracleCase,
    name: String,
    hjkl: hjkl_driver::HjklOutcome,
) -> CaseResult {
    if hjkl.buffer != case.expected_buffer {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_buffer",
                expected: case.expected_buffer.clone(),
                got_hjkl: hjkl.buffer,
                got_nvim: "nvim absent — comparing against authored expectation".to_string(),
            },
        };
    }
    if let Some(expected) = case.expected_cursor
        && hjkl.cursor != expected
    {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_cursor",
                expected: format!("{expected:?}"),
                got_hjkl: format!("{:?}", hjkl.cursor),
                got_nvim: "nvim absent — comparing against authored expectation".to_string(),
            },
        };
    }
    if let Some(expected) = case.expected_mode.as_deref()
        && hjkl.mode != expected
    {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_mode",
                expected: expected.to_string(),
                got_hjkl: hjkl.mode,
                got_nvim: "nvim absent — comparing against authored expectation".to_string(),
            },
        };
    }
    if let Some((_, expected)) = case.expected_register.as_ref()
        && hjkl.pinned_register.as_deref() != Some(expected.as_str())
    {
        return CaseResult {
            name,
            status: CaseStatus::Mismatch {
                field: "expected_register",
                expected: expected.clone(),
                got_hjkl: format!("{:?}", hjkl.pinned_register),
                got_nvim: "nvim absent — comparing against authored expectation".to_string(),
            },
        };
    }
    CaseResult {
        name,
        status: CaseStatus::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OracleCase;

    fn case() -> OracleCase {
        OracleCase {
            name: "no_nvim_fallback".into(),
            initial_buffer: "ab\n".into(),
            initial_cursor: (0, 0),
            keys: String::new(),
            expected_buffer: "ab\n".into(),
            expected_cursor: Some((0, 0)),
            expected_mode: Some("normal".into()),
            expected_register: None,
            shiftwidth: None,
            expandtab: None,
            textwidth: None,
            autoindent: None,
            foldmethod: None,
        }
    }

    fn outcome(buffer: &str, cursor: (usize, usize), mode: &str) -> hjkl_driver::HjklOutcome {
        hjkl_driver::HjklOutcome {
            buffer: buffer.into(),
            cursor,
            mode: mode.into(),
            default_register: String::new(),
            pinned_register: None,
        }
    }

    /// hjkl matching the authored expectations passes without nvim present.
    #[test]
    fn no_nvim_fallback_passes_on_match() {
        let result = check_against_authored(&case(), "t".into(), outcome("ab\n", (0, 0), "normal"));
        assert!(matches!(result.status, CaseStatus::Pass), "{result:?}");
    }

    /// A divergent buffer is a real mismatch, not a skip — this is the whole
    /// point of the fallback (the corpus previously guarded nothing here).
    #[test]
    fn no_nvim_fallback_flags_buffer_mismatch() {
        let result = check_against_authored(&case(), "t".into(), outcome("XY\n", (0, 0), "normal"));
        assert!(
            matches!(result.status, CaseStatus::Mismatch { field, .. } if field == "expected_buffer"),
            "{result:?}"
        );
    }

    #[test]
    fn no_nvim_fallback_flags_cursor_mismatch() {
        let result = check_against_authored(&case(), "t".into(), outcome("ab\n", (0, 1), "normal"));
        assert!(
            matches!(result.status, CaseStatus::Mismatch { field, .. } if field == "expected_cursor"),
            "{result:?}"
        );
    }

    #[test]
    fn no_nvim_fallback_flags_mode_mismatch() {
        let result = check_against_authored(&case(), "t".into(), outcome("ab\n", (0, 0), "insert"));
        assert!(
            matches!(result.status, CaseStatus::Mismatch { field, .. } if field == "expected_mode"),
            "{result:?}"
        );
    }
}
