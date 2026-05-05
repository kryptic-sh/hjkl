use std::path::PathBuf;

#[tokio::test(flavor = "multi_thread")]
async fn sample_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/sample.toml");
    let corpus = hjkl_compat_oracle::load_corpus(&corpus_path).unwrap();

    if !hjkl_compat_oracle::nvim_available() {
        eprintln!("skipping: nvim not installed");
        return;
    }

    let results = hjkl_compat_oracle::run_oracle(&corpus).await;

    let failures: Vec<_> = results
        .iter()
        .filter(|r| {
            !matches!(
                r.status,
                hjkl_compat_oracle::CaseStatus::Pass | hjkl_compat_oracle::CaseStatus::Skipped(_)
            )
        })
        .collect();

    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tier1_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier1.toml");
    let corpus = hjkl_compat_oracle::load_corpus(&corpus_path).unwrap();

    if !hjkl_compat_oracle::nvim_available() {
        eprintln!("skipping: nvim not installed");
        return;
    }

    let results = hjkl_compat_oracle::run_oracle(&corpus).await;

    let failures: Vec<_> = results
        .iter()
        .filter(|r| {
            !matches!(
                r.status,
                hjkl_compat_oracle::CaseStatus::Pass | hjkl_compat_oracle::CaseStatus::Skipped(_)
            )
        })
        .collect();

    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn known_divergences_report() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/known_divergences.toml");
    let corpus = hjkl_compat_oracle::load_corpus(&corpus_path).unwrap();

    if !hjkl_compat_oracle::nvim_available() {
        eprintln!("skipping: nvim not installed");
        return;
    }

    let results = hjkl_compat_oracle::run_oracle(&corpus).await;

    let pass_count = results
        .iter()
        .filter(|r| matches!(r.status, hjkl_compat_oracle::CaseStatus::Pass))
        .count();
    let mismatch_count = results
        .iter()
        .filter(|r| {
            !matches!(
                r.status,
                hjkl_compat_oracle::CaseStatus::Pass | hjkl_compat_oracle::CaseStatus::Skipped(_)
            )
        })
        .count();

    eprintln!(
        "known_divergences report: {}/{} cases pass (mismatch: {})",
        pass_count,
        results.len(),
        mismatch_count
    );

    let newly_passing: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.status, hjkl_compat_oracle::CaseStatus::Pass))
        .collect();

    if newly_passing.is_empty() {
        eprintln!("  no divergences fixed yet");
    } else {
        eprintln!("  cases now passing (divergences fixed):");
        for r in &newly_passing {
            eprintln!("    ✓ {}", r.name);
        }
    }

    // Never fails — report only.
}

/// Drive the nvim-api tier corpus through `hjkl --nvim-api` and assert every
/// case passes. No env gate — always runs.
///
/// If the hjkl binary doesn't exist (e.g. bare `cargo test -p hjkl-compat-oracle`
/// without a prior build), the test skips with an `eprintln!` rather than
/// failing.
///
/// Binary resolution order:
/// 1. `HJKL_BIN` environment variable.
/// 2. `<workspace>/target/debug/hjkl{EXE_SUFFIX}` derived from `CARGO_MANIFEST_DIR`.
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_tier_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/nvim_api_tier.toml");
    let corpus = hjkl_compat_oracle::load_corpus(&corpus_path).unwrap();

    // Resolve binary path using the same logic as hjkl_driver, but check
    // existence here so we can skip gracefully.
    let bin_path: std::path::PathBuf = if let Ok(v) = std::env::var("HJKL_BIN") {
        v.into()
    } else {
        let exe_name = format!("hjkl{}", std::env::consts::EXE_SUFFIX);
        std::path::Path::new(manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .map(|p| p.join("target/debug").join(&exe_name))
            .unwrap_or_else(|| std::path::PathBuf::from(&exe_name))
    };

    if !bin_path.exists() {
        eprintln!(
            "skipping nvim_api_tier_passes: binary not found at {}. \
             Run `cargo build -p hjkl --bin hjkl` first, or set HJKL_BIN.",
            bin_path.display()
        );
        return;
    }

    let mut failures: Vec<String> = Vec::new();

    for case in &corpus.cases {
        match hjkl_compat_oracle::hjkl_driver::run_case_via_nvim_api(case).await {
            Err(e) => {
                failures.push(format!("{}: driver error: {e}", case.name));
            }
            Ok(outcome) => {
                // Re-apply trailing newline convention.
                let mut buf = outcome.buffer.clone();
                if case.initial_buffer.ends_with('\n') && !buf.ends_with('\n') {
                    buf.push('\n');
                }
                if buf != case.expected_buffer {
                    failures.push(format!(
                        "{}: buffer mismatch\n  expected: {:?}\n  got:      {:?}",
                        case.name, case.expected_buffer, buf
                    ));
                }
                if let Some(expected_cursor) = case.expected_cursor
                    && outcome.cursor != expected_cursor
                {
                    failures.push(format!(
                        "{}: cursor mismatch: expected {:?}, got {:?}",
                        case.name, expected_cursor, outcome.cursor
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "nvim_api_tier cases failed:\n{}",
        failures.join("\n")
    );
}
