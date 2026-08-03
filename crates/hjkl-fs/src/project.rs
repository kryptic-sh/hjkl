//! One answer to "which files are part of this project".
//!
//! The explorer tree, the fuzzy file picker, the fuzzy grep picker and `:grep`
//! all enumerate the user's files, and each used to decide for itself what to
//! skip: the explorer asked `git2` per entry, the file picker built an
//! [`ignore::WalkBuilder`] with hidden files off, and the two grep paths took
//! ripgrep's defaults. The three answers disagreed — a dotfile was listed by
//! the explorer, invisible to the picker, and unsearchable by `:grep` — so
//! finding a file depended on which surface you happened to reach for.
//!
//! # The policy
//!
//! **Everything git would show you, plus dotfiles, minus `.git` itself.** In
//! other words a path is part of the project when it is tracked, or untracked
//! and not ignored:
//!
//! - **Ignore files are honoured** — `.gitignore`, `.ignore`,
//!   `.git/info/exclude` and the global `core.excludesfile`, including the ones
//!   in parent directories above the walk root. Ignored directories are pruned
//!   rather than walked, which is what keeps `target/` and `node_modules/` from
//!   dominating the cost.
//! - **Hidden files are shown and searched.** A leading dot is a naming
//!   convention, not a statement that the file is not yours: `.github/`,
//!   `.cargo/config.toml` and `.env.example` are all project files you want to
//!   open and grep.
//! - **`.git` itself is always skipped.** It is neither hidden-by-convention nor
//!   ignored (git does not ignore its own directory), so once dotfiles are shown
//!   it has to be excluded by name or every grep drowns in packfiles and every
//!   file list carries hundreds of refs.
//!
//! # Two engines, one policy
//!
//! Surfaces that walk the tree in-process use [`walk_builder`]. Surfaces that
//! shell out to ripgrep pass [`RG_IGNORE_ARGS`], which spells the same three
//! rules in ripgrep's own flags. They are separate expressions of one decision,
//! so [`rg_args_match_walk_policy`] is a test, not a comment.

use std::path::{Path, PathBuf};

/// The repository's internal directory — excluded from every walk and search.
///
/// Not a general "hidden directory" rule: this is the one name that is both
/// unignored by git and never something the user means to open or grep.
pub const GIT_DIR: &str = ".git";

/// Ripgrep flags that reproduce the module policy for the shell-out surfaces.
///
/// `--hidden` turns dotfiles back on (ripgrep skips them by default) and
/// `--glob !.git` takes `.git` back out, which `--hidden` would otherwise
/// expose. Ignore-file handling needs no flag: honouring them is ripgrep's
/// default, and the surfaces deliberately do not pass `--no-ignore`.
pub const RG_IGNORE_ARGS: &[&str] = &["--hidden", "--glob", "!.git"];

/// A walker over `root` configured with the module policy.
///
/// The caller owns everything else — depth, threads, whether directories are
/// yielded — so a lazy one-level listing and a full recursive scan can share
/// the ignore rules without sharing a traversal strategy.
///
/// `.git` is filtered by name here rather than through an
/// [`ignore::overrides::Override`] so that the exclusion also prunes the
/// subtree: a filtered-out directory is never descended into.
pub fn walk_builder(root: &Path) -> ignore::WalkBuilder {
    let mut b = ignore::WalkBuilder::new(root);
    b.hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .filter_entry(|e| e.file_name() != GIT_DIR);
    b
}

/// One directory's immediate children under the module policy, as
/// `(path, is_dir)` pairs in no particular order.
///
/// This is the lazy counterpart of [`walk_builder`] for a tree view that reads
/// only the directories the user has expanded. It still costs the ignore-file
/// stack above `dir` (`parents(true)`), which is what makes a child's verdict
/// here identical to the one a full walk from the repository root would reach.
///
/// An unreadable `dir` yields an empty list rather than an error: a tree view
/// draws the directory as empty either way, and the same is true of a
/// permission-denied entry inside it.
pub fn list_dir(dir: &Path) -> Vec<(PathBuf, bool)> {
    walk_builder(dir)
        .max_depth(Some(1))
        .build()
        .filter_map(Result::ok)
        // Depth 0 is `dir` itself, which the caller already has.
        .filter(|e| e.depth() > 0)
        .map(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            (e.into_path(), is_dir)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;

    /// A tree with one of everything the policy has an opinion about.
    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        fs::write(r.join(".gitignore"), "ignored/\nskip.txt\n").unwrap();
        fs::create_dir_all(r.join(".git")).unwrap();
        fs::write(r.join(".git").join("config"), "x").unwrap();
        fs::create_dir_all(r.join("ignored")).unwrap();
        fs::write(r.join("ignored").join("a.txt"), "x").unwrap();
        fs::create_dir_all(r.join(".github")).unwrap();
        fs::write(r.join(".github").join("ci.yml"), "x").unwrap();
        fs::write(r.join(".hidden"), "x").unwrap();
        fs::write(r.join("skip.txt"), "x").unwrap();
        fs::write(r.join("keep.txt"), "x").unwrap();
        tmp
    }

    /// Every path a full walk yields, relative to the root, slash-joined.
    fn walked(root: &Path) -> BTreeSet<String> {
        walk_builder(root)
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.depth() > 0)
            .map(|e| {
                e.path()
                    .strip_prefix(root)
                    .unwrap_or_else(|_| e.path())
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    #[test]
    fn walk_shows_dotfiles_and_skips_ignored_and_git() {
        let tmp = fixture();
        let got = walked(tmp.path());

        // Hidden, not ignored ⇒ part of the project.
        assert!(got.contains(".hidden"), "{got:?}");
        assert!(got.contains(".github"), "{got:?}");
        assert!(got.contains(".github/ci.yml"), "{got:?}");
        assert!(got.contains(".gitignore"), "{got:?}");
        assert!(got.contains("keep.txt"), "{got:?}");

        // Ignored ⇒ out, and the directory is pruned rather than descended.
        assert!(!got.contains("skip.txt"), "{got:?}");
        assert!(!got.contains("ignored"), "{got:?}");
        assert!(!got.contains("ignored/a.txt"), "{got:?}");

        // `.git` ⇒ out, subtree included.
        assert!(!got.iter().any(|p| p.starts_with(".git/")), "{got:?}");
        assert!(!got.contains(".git"), "{got:?}");
    }

    #[test]
    fn list_dir_matches_the_walk_at_depth_one() {
        let tmp = fixture();
        let got: BTreeSet<String> = list_dir(tmp.path())
            .into_iter()
            .map(|(p, _)| {
                p.strip_prefix(tmp.path())
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let want: BTreeSet<String> = walked(tmp.path())
            .into_iter()
            .filter(|p| !p.contains('/'))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn list_dir_reports_directories() {
        let tmp = fixture();
        let entries = list_dir(tmp.path());
        let github = entries
            .iter()
            .find(|(p, _)| p.file_name().is_some_and(|n| n == ".github"))
            .expect(".github must be listed");
        assert!(github.1, "`.github` is a directory");
        let keep = entries
            .iter()
            .find(|(p, _)| p.file_name().is_some_and(|n| n == "keep.txt"))
            .expect("keep.txt must be listed");
        assert!(!keep.1, "`keep.txt` is a file");
    }

    #[test]
    fn list_dir_on_an_unreadable_path_is_empty_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_dir(&tmp.path().join("does-not-exist")).is_empty());
    }

    /// A nested `.gitignore` applies to the directory it sits in, and a lazy
    /// `list_dir` of a SUBDIRECTORY still sees the root's rules — that is what
    /// `parents(true)` buys, and it is the difference between the tree view
    /// agreeing with the pickers and quietly diverging inside subdirectories.
    #[test]
    fn nested_and_parent_ignore_rules_both_apply_to_a_lazy_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        // `.gitignore` rules apply only inside a repository (ripgrep behaves
        // the same way), so the marker directory is part of the fixture.
        fs::create_dir_all(r.join(".git")).unwrap();
        fs::write(r.join(".gitignore"), "from-root.txt\n").unwrap();
        fs::create_dir_all(r.join("sub")).unwrap();
        fs::write(r.join("sub").join(".gitignore"), "from-sub.txt\n").unwrap();
        fs::write(r.join("sub").join("from-root.txt"), "x").unwrap();
        fs::write(r.join("sub").join("from-sub.txt"), "x").unwrap();
        fs::write(r.join("sub").join("kept.txt"), "x").unwrap();

        let names: BTreeSet<String> = list_dir(&r.join("sub"))
            .into_iter()
            .filter_map(|(p, _)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();

        assert!(names.contains("kept.txt"), "{names:?}");
        assert!(!names.contains("from-sub.txt"), "nested rule: {names:?}");
        assert!(!names.contains("from-root.txt"), "parent rule: {names:?}");
    }

    /// The ripgrep spelling and the walker are two expressions of one policy,
    /// so they are checked against each other rather than trusted to stay in
    /// step. Skipped when ripgrep is absent — the surfaces that use the flags
    /// fall back to `grep`/`findstr` there anyway.
    #[test]
    fn rg_args_match_walk_policy() {
        let tmp = fixture();
        let root = tmp.path();
        // Give every file the same searchable content so the comparison is
        // about which files are VISITED, not which ones happen to match.
        for (p, is_dir) in walk_builder(root)
            .build()
            .filter_map(Result::ok)
            .map(|e| {
                (
                    e.path().to_path_buf(),
                    e.file_type().is_some_and(|t| t.is_dir()),
                )
            })
            .collect::<Vec<_>>()
        {
            if !is_dir {
                fs::write(&p, "needle\n").unwrap();
            }
        }
        for p in [
            root.join("ignored").join("a.txt"),
            root.join("skip.txt"),
            root.join(".git").join("config"),
        ] {
            fs::write(&p, "needle\n").unwrap();
        }

        let mut cmd = std::process::Command::new("rg");
        cmd.args(RG_IGNORE_ARGS)
            .args(["--no-config", "--files-with-matches", "--", "needle"])
            .current_dir(root);
        let Ok(out) = cmd.output() else {
            eprintln!("ripgrep not installed — skipping");
            return;
        };
        let from_rg: BTreeSet<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim_start_matches("./").replace('\\', "/"))
            .collect();

        let from_walk: BTreeSet<String> = walk_builder(root)
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .map(|e| {
                e.path()
                    .strip_prefix(root)
                    .unwrap_or_else(|_| e.path())
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();

        assert_eq!(
            from_rg, from_walk,
            "`RG_IGNORE_ARGS` and `walk_builder` must select the same files"
        );
    }
}
