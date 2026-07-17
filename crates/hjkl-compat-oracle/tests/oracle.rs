use std::path::PathBuf;

/// Load `rel_path` (relative to the crate manifest), run it through the oracle,
/// and assert every case passes (or is skipped). Skips entirely when nvim is
/// not on PATH.
async fn run_corpus(rel_path: &str) {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join(rel_path);
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

/// Resolve the `hjkl` binary path (HJKL_BIN override, else workspace
/// target/debug). Returns `None` if it doesn't exist so callers can skip.
fn resolve_hjkl_bin() -> Option<std::path::PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let bin_path: std::path::PathBuf = if let Ok(v) = std::env::var("HJKL_BIN") {
        v.into()
    } else {
        let exe_name = format!("hjkl{}", std::env::consts::EXE_SUFFIX);
        std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("target/debug").join(&exe_name))
            .unwrap_or_else(|| std::path::PathBuf::from(&exe_name))
    };
    bin_path.exists().then_some(bin_path)
}

/// Drive `rel_path` through the `hjkl --nvim-api` subprocess and assert every
/// case's buffer (and cursor, when pinned) matches the authored expectation.
/// Skips when the binary isn't built. Used for `:`-ex / substitute corpora
/// that need the command line, not in-process key replay.
async fn run_corpus_via_nvim_api(rel_path: &str, label: &str) {
    // #264 (fixed): the nvim-api subprocess oracle spawns a `hjkl --nvim-api`
    // child per case. These used to hang on the display-less ubuntu CI runner —
    // root cause was the child's clipboard probe falling back to OSC 52, which
    // writes escapes to stdout (the rpc pipe) and desyncs the protocol. Rpc
    // modes now run clipboard-free (host::disable_clipboard_for_rpc), so these
    // run unconditionally again. The binary-not-found skip below + the nextest
    // slow-timeout remain as backstops.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join(rel_path);
    let corpus = hjkl_compat_oracle::load_corpus(&corpus_path).unwrap();

    let Some(bin_path) = resolve_hjkl_bin() else {
        eprintln!(
            "skipping {label}: hjkl binary not found. \
             Run `cargo build -p hjkl --bin hjkl` first, or set HJKL_BIN."
        );
        return;
    };
    let _ = bin_path;

    let mut failures: Vec<String> = Vec::new();

    for case in &corpus.cases {
        match hjkl_compat_oracle::hjkl_driver::run_case_via_nvim_api(case).await {
            Err(e) => {
                failures.push(format!("{}: driver error: {e}", case.name));
            }
            Ok(outcome) => {
                let mut buf = outcome.buffer.clone();
                // Don't fabricate a trailing newline for a fully-emptied
                // buffer — see the matching guard in hjkl_driver.rs (H1).
                if case.initial_buffer.ends_with('\n') && !buf.is_empty() && !buf.ends_with('\n') {
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
        "{label} cases failed:\n{}",
        failures.join("\n")
    );
}

/// Drive the tier-2 substitute corpus through `hjkl --nvim-api`. Mirrors
/// the `nvim_api_tier_passes` shape because `:` ex commands need the
/// nvim-api subprocess driver, not the in-process key replay.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_substitute_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_substitute.toml", "tier2_substitute").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_ex_lineops_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_ex_lineops.toml", "tier2_ex_lineops").await;
}

/// B8/B9: vim default-magic regex translation (`\( \) \+ \? \= \| \{n,m}`,
/// the literal-unless-escaped inverse, and the `\v`/`\V` mode switches).
/// `:s` cases need the nvim-api subprocess driver (ex commands aren't
/// dispatched by in-process key replay); the plain `/` search cases in this
/// file are routed to `nvim_input` by the same driver's non-`:` fallback
/// (see `hjkl_driver::run_case_via_nvim_api_inner`), so one driver covers
/// the whole corpus.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_regex_magic_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_regex_magic.toml", "tier2_regex_magic").await;
}

/// B17: bare `:s` (no `/pattern/replacement/`) repeats the last substitute's
/// pattern AND replacement — see `substitute_handler`'s bare-form branch.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_bare_s_repeat_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_bare_s_repeat.toml", "tier2_bare_s_repeat").await;
}

/// B5: `:[range]j[oin][!] [count]` ex command.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_ex_join_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_ex_join.toml", "tier2_ex_join").await;
}

/// B6: `:[range]y[ank] [{register}] [count]` ex command.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_ex_yank_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_ex_yank.toml", "tier2_ex_yank").await;
}

/// B7: `:[range]d[elete] [{register}] [count]` writes registers.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_ex_delete_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_ex_delete.toml", "tier2_ex_delete").await;
}

/// B4/B14: `:g`/`:v` sub-command execution with vim's two-pass model plus
/// register/cursor semantics.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_ex_global_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_ex_global.toml", "tier2_ex_global").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_case_indent_join_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_case_indent_join.toml");
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
async fn tier2_indent_count_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_indent_count.toml");
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
async fn tier2_count_open_corpus_passes() {
    run_corpus("corpus/tier2_count_open.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_space_motion_corpus_passes() {
    run_corpus("corpus/tier2_space_motion.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_motion_coverage_corpus_passes() {
    run_corpus("corpus/tier2_motion_coverage.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_autoindent_open_corpus_passes() {
    run_corpus("corpus/tier2_autoindent_open.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_fold_delete_corpus_passes() {
    run_corpus("corpus/tier2_fold_delete.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_registers_corpus_passes() {
    run_corpus("corpus/tier2_registers.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_paste_family_corpus_passes() {
    run_corpus("corpus/tier2_paste_family.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_gn_corpus_passes() {
    run_corpus("corpus/tier2_gn.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_incr_case_corpus_passes() {
    run_corpus("corpus/tier2_incr_case.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_indent_ops_corpus_passes() {
    run_corpus("corpus/tier2_indent_ops.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_registers_special_corpus_passes() {
    run_corpus("corpus/tier2_registers_special.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_visual_join_corpus_passes() {
    run_corpus("corpus/tier2_visual_join.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_percent_motion_corpus_passes() {
    run_corpus("corpus/tier2_percent_motion.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_insert_ctrl_r_corpus_passes() {
    run_corpus("corpus/tier2_insert_ctrl_r.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_unmatched_brackets_corpus_passes() {
    run_corpus("corpus/tier2_unmatched_brackets.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_increment_count_corpus_passes() {
    run_corpus("corpus/tier2_increment_count.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_replace_mode_corpus_passes() {
    run_corpus("corpus/tier2_replace_mode.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_op_search_corpus_passes() {
    run_corpus("corpus/tier2_op_search.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_undo_redo_corpus_passes() {
    run_corpus("corpus/tier2_undo_redo.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_undo_combo_corpus_passes() {
    run_corpus("corpus/tier2_undo_combo.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_rot13_corpus_passes() {
    run_corpus("corpus/tier2_rot13.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_more_coverage_corpus_passes() {
    run_corpus("corpus/tier2_more_coverage.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_ex_undo_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_ex_undo.toml", "tier2_ex_undo").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_abbrev_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_abbrev.toml", "tier2_abbrev").await;
}

// NOTE: `@:` (repeat last ex) is implemented in the TUI app chord layer
// (route_chord_key / replay_last_ex) with unit tests in
// apps/hjkl/src/app/tests/marks_registers.rs. It is NOT oracle-tested because
// the `hjkl --nvim-api` driver feeds keys straight to the engine and bypasses
// the app chord layer where `@:` lives.

#[tokio::test(flavor = "multi_thread")]
async fn tier2_search_offsets_corpus_passes() {
    run_corpus("corpus/tier2_search_offsets.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_jumps_corpus_passes() {
    run_corpus("corpus/tier2_jumps.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_reflow_corpus_passes() {
    run_corpus("corpus/tier2_reflow.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_text_objects_edge_corpus_passes() {
    run_corpus("corpus/tier2_text_objects_edge.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_block_textobj_corpus_passes() {
    run_corpus("corpus/tier2_block_textobj.toml").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_paragraph_word_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_paragraph_word.toml");
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
async fn tier2_macros_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_macros.toml");
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
async fn tier2_search_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_search.toml");
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
async fn tier2_dot_repeat_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_dot_repeat.toml");
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
async fn tier2_visual_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_visual.toml");
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
async fn tier2_advanced_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_advanced.toml");
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
async fn tier2_visual_block_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_visual_block.toml");
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
async fn tier2_text_objects_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_text_objects.toml");
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
async fn tier2_change_yank_objects_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_change_yank_objects.toml");
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
async fn tier2_marks_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_marks.toml");
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
    // #264 (fixed) — see run_corpus_via_nvim_api. The ubuntu hang was the child's
    // OSC 52 clipboard fallback corrupting the rpc stream; rpc modes now run
    // clipboard-free, so this runs unconditionally on all platforms again.
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
                // Re-apply trailing newline convention. Skip for a
                // fully-emptied buffer — see the matching guard in
                // hjkl_driver.rs (H1).
                let mut buf = outcome.buffer.clone();
                if case.initial_buffer.ends_with('\n') && !buf.is_empty() && !buf.ends_with('\n') {
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

#[tokio::test(flavor = "multi_thread")]
async fn tier2_gaps_corpus_passes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_gaps.toml");
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

/// Oracle B: drive the sneak-disabled fallback corpus via `hjkl --nvim-api`.
///
/// Sneak-ON behavior (s+2chars digraph jump) is vim-sneak plugin behavior and
/// deliberately NOT tested here — nvim's default `s` is substitute-char and
/// diverges from sneak-ON mode by design. Only the `:set nomotion_sneak`
/// fallback path is compared against nvim's substitute-char behavior.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_sneak_disabled_fallback_corpus_passes() {
    // #264 (fixed) — spawns a `hjkl --nvim-api` child. The ubuntu hang was the
    // child's OSC 52 clipboard fallback corrupting the rpc stream; rpc modes now
    // run clipboard-free, so this runs unconditionally on all platforms again.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_path = PathBuf::from(manifest_dir).join("corpus/tier2_sneak.toml");
    let corpus = hjkl_compat_oracle::load_corpus(&corpus_path).unwrap();

    let bin_path: std::path::PathBuf = if let Ok(v) = std::env::var("HJKL_BIN") {
        v.into()
    } else {
        let exe_name = format!("hjkl{}", std::env::consts::EXE_SUFFIX);
        std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("target/debug").join(&exe_name))
            .unwrap_or_else(|| std::path::PathBuf::from(&exe_name))
    };

    if !bin_path.exists() {
        eprintln!(
            "skipping tier2_sneak_disabled_fallback_corpus_passes: binary not found at {}. \
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
                let mut buf = outcome.buffer.clone();
                if case.initial_buffer.ends_with('\n') && !buf.is_empty() && !buf.ends_with('\n') {
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
        "tier2_sneak_disabled_fallback cases failed:\n{}",
        failures.join("\n")
    );
}

/// B1: vertical motions must not step onto ropey's phantom trailing row.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_vertical_phantom_row_corpus_passes() {
    run_corpus("corpus/tier2_vertical_phantom_row.toml").await;
}

/// B2: `M` / `L` viewport-relative motions, previously untestable because
/// the engine's viewport height got clobbered to 0 on headless hosts.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_viewport_bounds_corpus_passes() {
    run_corpus("corpus/tier2_viewport_bounds.toml").await;
}

/// B3: `(` / `)` sentence motion blank-line / closing-punctuation / EOF
/// boundary rules.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_sentence_corpus_passes() {
    run_corpus("corpus/tier2_sentence.toml").await;
}

/// Round 2b hardening pass — B11/B12/B13/B16/B19/B20/B21 + insert C-a/C-e/C-y
/// (B1), insert C-w/C-u (B2/B3), and linewise case-op blank-line (B10).
#[tokio::test(flavor = "multi_thread")]
async fn tier2_round2b_corpus_passes() {
    run_corpus("corpus/tier2_round2b.toml").await;
}

/// H1: oracle harness fix pins — cases that fully empty the buffer, only
/// testable after the nvim_driver.rs / hjkl_driver.rs trailing-newline fix.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_round3_h1_corpus_passes() {
    run_corpus("corpus/tier2_round3_h1.toml").await;
}

/// H1 (nvim-api arm): `:%d` fully empties the buffer via the ex command
/// path.
#[tokio::test(flavor = "multi_thread")]
async fn tier2_round3_h1_ex_corpus_passes() {
    run_corpus_via_nvim_api("corpus/tier2_round3_h1_ex.toml", "tier2_round3_h1_ex").await;
}

/// H2: linewise operator deletes must clamp the cursor when the deleted
/// range reaches the buffer end (`run_operator_over_range`'s Delete arm,
/// reached by motion-driven deletes like dG/dj, was missing the phantom-row
/// clamp that the dedicated dd path already had).
#[tokio::test(flavor = "multi_thread")]
async fn tier2_round3_h2_corpus_passes() {
    run_corpus("corpus/tier2_round3_h2.toml").await;
}

// B5 (`U` / undo-line) is NOT oracle-tested: the nvim comparison side seeds
// each case's buffer via `nvim_buf_set_lines`, which real nvim's undo
// system treats as a genuine change — `U`'s restore-target line
// (`b_u_line_ptr`) ends up pointing at the pre-seed empty buffer instead of
// the seeded content, an artifact of the RPC-based seeding rather than real
// vim behaviour (confirmed against `nvim --headless <file> -c 'normal! ...'`,
// which does not go through that RPC path and behaves as expected). Pinned
// instead as unit tests in `crates/hjkl-vim/tests/undo_line.rs`, each
// annotated with the real-nvim-file invocation it was verified against.
