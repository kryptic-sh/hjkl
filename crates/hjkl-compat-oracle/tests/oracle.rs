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
