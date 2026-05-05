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

/// When `HJKL_ORACLE_NVIM_API=1` is set, drive the substitute cases (which
/// previously lived in `known_divergences.toml`) through `hjkl --nvim-api`
/// and assert they pass. The cases are now in `tier1.toml` under the
/// "substitute" group; we filter by name prefix to run only those.
///
/// Run with:
/// ```sh
/// HJKL_ORACLE_NVIM_API=1 cargo test -p hjkl-compat-oracle substitute_via_nvim_api
/// ```
#[tokio::test(flavor = "multi_thread")]
async fn substitute_via_nvim_api() {
    if std::env::var("HJKL_ORACLE_NVIM_API").as_deref() != Ok("1") {
        eprintln!("skipping: HJKL_ORACLE_NVIM_API not set to 1");
        return;
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/known_divergences.toml");
    let corpus = hjkl_compat_oracle::load_corpus(&corpus_path).unwrap();

    // Run only the substitute_ cases (the ones that diverge in-process but
    // pass via the nvim-api driver).
    let sub_cases: Vec<_> = corpus
        .cases
        .iter()
        .filter(|c| c.name.starts_with("substitute_"))
        .collect();

    if sub_cases.is_empty() {
        eprintln!("no substitute_ cases found in tier1.toml");
        return;
    }

    let mut failures: Vec<String> = Vec::new();

    for case in &sub_cases {
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
        "substitute cases failed via nvim-api:\n{}",
        failures.join("\n")
    );
}
