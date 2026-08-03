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

    if !nvim_ok {
        return CaseResult {
            name,
            status: CaseStatus::Skipped("nvim not installed".to_string()),
        };
    }

    // Run hjkl driver.
    let hjkl = match hjkl_driver::run_case(case) {
        Ok(o) => o,
        Err(e) => {
            return CaseResult {
                name,
                status: CaseStatus::HjklError(e.to_string()),
            };
        }
    };

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

    // Compare cursor.
    // Note: hjkl cursor col is char-indexed; nvim cursor col is byte-indexed.
    // For ASCII-only test cases these are equivalent. Differences on non-ASCII
    // content will surface here naturally.
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
