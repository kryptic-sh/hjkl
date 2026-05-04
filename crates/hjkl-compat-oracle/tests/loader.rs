use hjkl_compat_oracle::load_corpus;
use std::path::Path;

#[test]
fn sample_corpus_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/sample.toml");
    let corpus = load_corpus(&path).expect("failed to load sample corpus");
    assert_eq!(corpus.cases.len(), 2);
    assert_eq!(corpus.cases[0].name, "noop");
    assert_eq!(corpus.cases[1].keys, "l");
}
