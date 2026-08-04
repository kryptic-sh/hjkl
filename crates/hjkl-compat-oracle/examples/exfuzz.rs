//! Randomised ex-command differential fuzzer: hjkl vs neovim.
//!
//! Generates random ASCII buffers and random EX-COMMAND sequences, replays
//! each through both engines, and reports every divergence. The hjkl side
//! runs IN-PROCESS through [`hjkl_ex::try_dispatch`] on an
//! [`hjkl_engine::Editor`] built exactly like `hjkl_driver::run_case` does;
//! the nvim side spawns a headless nvim, seeds it identically (buffer +
//! cursor + option pins + cleared undo history, via
//! `nvim_driver::run_commands`), and runs each command through
//! `nvim.command`. Both engines start from empty undo trees, so `u` /
//! `<C-r>` in `:normal` keys diff honestly.
//!
//! Usage:
//!   cargo run -p hjkl-compat-oracle --release --example exfuzz -- [CASES] [SEED]
//!
//! Command grammar — every shape verified to dispatch on both sides
//! (`tests/harness_probes.rs`; a shape that errored on either side was
//! dropped):
//!   d | y | j                      — delete / yank / join the cursor line
//!   {2,3}d | {2,3}y | {2,3}j       — line op addressed at that line
//!   s/pat/rep/ | s/pat/rep/g       — substitute on the cursor line
//!   g/pat/d | v/pat/d              — global / vglobal line delete
//!   normal {keys}                  — 1-3 keys concatenated from
//!                                     {j, k, x, dd, A!}
//!   (`normal p` was dropped: nvim errors E353 on an empty register.
//!   `A!<Esc>` was dropped too: nvim doesn't expand `<Esc>` key-notation
//!   inside `nvim.command` strings, so the shape could never diff honestly.)
//!
//! Divergences are printed corpus-TOML-ready (initial_buffer /
//! initial_cursor / keys / expected_*), like difffuzz's report.

use hjkl_compat_oracle::{OracleCase, hjkl_driver::HjklOutcome, nvim_driver, test_host::TestHost};
use hjkl_engine::{Options, VimMode};
use hjkl_vim::VimEditorExt;

// ── tiny deterministic PRNG (no dep; same xorshift64* as difffuzz) ──────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

// ── buffer generation (mirrors difffuzz) ────────────────────────────────────

const WORDS: &[&str] = &[
    "foo", "bar", "baz", "qux", "hello", "a", "xy", "n1", "_id", "self.x", "it's", "A-B", "end",
];

fn gen_line(rng: &mut Rng) -> String {
    let mut line = String::new();
    if rng.chance(4) {
        line.push_str(rng.pick(&["  ", "    ", "\t"]));
    }
    let n = 1 + rng.below(4);
    for i in 0..n {
        if i > 0 {
            line.push_str(rng.pick(&[" ", "  ", ", ", "."]));
        }
        use std::fmt::Write as _;
        match rng.below(8) {
            0 => write!(line, "({})", rng.pick(WORDS)).unwrap(),
            1 => write!(line, "{{{}}}", rng.pick(WORDS)).unwrap(),
            2 => write!(line, "[{}]", rng.pick(WORDS)).unwrap(),
            3 => write!(line, "\"{}\"", rng.pick(WORDS)).unwrap(),
            4 => write!(line, "'{}'", rng.pick(WORDS)).unwrap(),
            _ => line.push_str(rng.pick(WORDS)),
        }
    }
    if rng.chance(6) {
        line.push_str(rng.pick(&[" ", "  ", "\t"]));
    }
    line
}

fn gen_buffer(rng: &mut Rng) -> String {
    let rows = 1 + rng.below(6);
    let mut lines: Vec<String> = Vec::new();
    for _ in 0..rows {
        if rng.chance(6) {
            lines.push(String::new());
        } else {
            lines.push(gen_line(rng));
        }
    }
    lines.join("\n")
}

// ── command generation ──────────────────────────────────────────────────────

/// Patterns / replacements for `:s` and `:g`. Plain words only — a regex
/// metacharacter in a pattern would exercise engine-specific regex dialects
/// rather than the ex layer itself.
const PAT_WORDS: &[&str] = &["foo", "bar", "baz", "a", "x", "hello", "n1", "end"];

/// Keys for `:normal` (concatenated, never space-separated, so the argument
/// is a single token stream on both sides). `p` is absent — `:normal p` on
/// an empty register errors on nvim (E353) — and so is `A!<Esc>`: nvim does
/// not expand `<Esc>` key-notation inside `nvim.command` strings (it inserts
/// the literal text), so the shape could never diff honestly. `A!` alone is
/// honest — `:normal` returns to Normal mode at the end of the invocation on
/// both engines.
const NORMAL_KEYS: &[&str] = &["j", "k", "x", "dd", "A!"];

/// Optional 2-3 count prefix for line ops.
fn count_str(rng: &mut Rng) -> String {
    if rng.chance(3) {
        (2 + rng.below(2)).to_string()
    } else {
        String::new()
    }
}

/// One self-contained ex command (without the leading `:`).
fn gen_command(rng: &mut Rng) -> String {
    match rng.below(7) {
        // Line ops, bare or addressed at line {2,3}.
        0..=2 => match rng.below(3) {
            0 => format!("{}d", count_str(rng)),
            1 => format!("{}y", count_str(rng)),
            _ => format!("{}j", count_str(rng)),
        },
        // Substitute on the cursor line.
        3 => format!("s/{}/{}/", rng.pick(PAT_WORDS), rng.pick(PAT_WORDS)),
        4 => format!("s/{}/{}/g", rng.pick(PAT_WORDS), rng.pick(PAT_WORDS)),
        // Global / vglobal delete.
        5 => format!("g/{}/d", rng.pick(PAT_WORDS)),
        _ => {
            let n = 1 + rng.below(3);
            let keys: String = (0..n).map(|_| *rng.pick(NORMAL_KEYS)).collect();
            format!("normal {keys}")
        }
    }
}

fn gen_commands(rng: &mut Rng) -> Vec<String> {
    let n = 1 + rng.below(4);
    (0..n).map(|_| gen_command(rng)).collect()
}

// ── case running ────────────────────────────────────────────────────────────

fn make_case(name: &str, buffer: &str, cursor: (usize, usize)) -> OracleCase {
    OracleCase {
        name: name.to_string(),
        initial_buffer: buffer.to_string(),
        initial_cursor: cursor,
        // Unused: exfuzz passes the commands separately, not as key replay.
        keys: String::new(),
        expected_buffer: String::new(),
        expected_cursor: None,
        expected_mode: None,
        expected_register: None,
        // Pin every setting where hjkl and `nvim --clean` disagree by default,
        // so a divergence means an engine bug rather than config skew — the
        // same pins difffuzz uses.
        shiftwidth: Some(4),
        expandtab: Some(true),
        textwidth: None,
        autoindent: Some(false),
        foldmethod: Some("manual".to_string()),
    }
}

/// Build an in-process editor exactly like `hjkl_driver::run_case` does.
fn build_editor(case: &OracleCase) -> hjkl_engine::Editor<hjkl_buffer::View, TestHost> {
    let buffer = hjkl_buffer::View::from_str(&case.initial_buffer);
    let mut editor = hjkl_vim::vim_editor(buffer, TestHost::new(), Options::default());
    editor.set_viewport_height(24);
    if let Some(sw) = case.shiftwidth {
        editor.settings_mut().shiftwidth = sw;
    }
    if let Some(et) = case.expandtab {
        editor.settings_mut().expandtab = et;
    }
    if let Some(tw) = case.textwidth {
        editor.settings_mut().textwidth = tw;
    }
    if let Some(ai) = case.autoindent {
        editor.settings_mut().autoindent = ai;
    }
    if let Some(ref fdm) = case.foldmethod {
        use hjkl_engine::types::FoldMethod;
        editor.settings_mut().foldmethod = match fdm.as_str() {
            "manual" => FoldMethod::Manual,
            "marker" => FoldMethod::Marker,
            _ => FoldMethod::Expr,
        };
    }
    let (init_row, init_col) = case.initial_cursor;
    editor.jump_cursor(init_row, init_col);
    editor
}

/// Read back hjkl state the same way `hjkl_driver::run_case` does.
fn read_hjkl(editor: &hjkl_engine::Editor<hjkl_buffer::View, TestHost>) -> HjklOutcome {
    let rope = editor.buffer().rope();
    let lines: Vec<String> = (0..rope.len_lines())
        .map(|i| hjkl_buffer::rope_line_str(&rope, i))
        .collect();
    let buffer_str = lines.join("\n");
    let cursor = editor.cursor();
    let mode = match editor.vim_mode() {
        VimMode::Normal => "normal",
        VimMode::Insert => "insert",
        VimMode::Visual => "visual",
        VimMode::VisualLine => "visual_line",
        VimMode::VisualBlock => "visual_block",
    }
    .to_string();
    HjklOutcome {
        buffer: buffer_str,
        cursor,
        mode,
        default_register: editor.yank(),
        pinned_register: None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Agree,
    Diverge {
        field: &'static str,
        hjkl: String,
        nvim: String,
    },
    /// nvim itself errored (a command it rejects) — not a signal about hjkl.
    NvimError,
    HjklPanic,
}

async fn judge(case: &OracleCase, cmds: &[String]) -> Verdict {
    let Ok(hjkl) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut editor = build_editor(case);
        let reg = hjkl_ex::default_registry::<TestHost>();
        for cmd in cmds {
            // None / Error / Unknown all leave the editor in its reported
            // state — whatever happened is what gets diffed.
            let _ = hjkl_ex::try_dispatch(&reg, &mut editor, cmd);
        }
        read_hjkl(&editor)
    })) else {
        return Verdict::HjklPanic;
    };

    let cmd_refs: Vec<&str> = cmds.iter().map(String::as_str).collect();
    let Ok(nvim) = nvim_driver::run_commands(case, &cmd_refs).await else {
        return Verdict::NvimError;
    };

    if hjkl.buffer != nvim.buffer {
        return Verdict::Diverge {
            field: "buffer",
            hjkl: format!("{:?}", hjkl.buffer),
            nvim: format!("{:?}", nvim.buffer),
        };
    }
    if hjkl.cursor != nvim.cursor {
        return Verdict::Diverge {
            field: "cursor",
            hjkl: format!("{:?}", hjkl.cursor),
            nvim: format!("{:?}", nvim.cursor),
        };
    }
    if hjkl.mode != nvim.mode {
        return Verdict::Diverge {
            field: "mode",
            hjkl: hjkl.mode,
            nvim: nvim.mode,
        };
    }
    if hjkl.default_register != nvim.default_register {
        return Verdict::Diverge {
            field: "register",
            hjkl: format!("{:?}", hjkl.default_register),
            nvim: format!("{:?}", nvim.default_register),
        };
    }
    Verdict::Agree
}

fn diverges(v: &Verdict) -> bool {
    matches!(v, Verdict::Diverge { .. } | Verdict::HjklPanic)
}

fn report(case: &OracleCase, cmds: &[String], verdict: &Verdict) {
    println!("\n─────────────────────────────────────────────────────────────");
    match verdict {
        Verdict::Diverge { field, hjkl, nvim } => {
            println!("DIVERGENCE [{field}]");
            println!("  hjkl: {hjkl}");
            println!("  nvim: {nvim}");
        }
        Verdict::HjklPanic => println!("HJKL PANIC / ERROR"),
        _ => unreachable!(),
    }
    println!("[[cases]]");
    println!("name = {:?}", case.name);
    println!("initial_buffer = {:?}", case.initial_buffer);
    println!(
        "initial_cursor = [{}, {}]",
        case.initial_cursor.0, case.initial_cursor.1
    );
    // Corpus keys form for the nvim-api driver: chained `:cmd<CR>` segments.
    let keys: String = cmds.iter().map(|c| format!(":{c}<CR>")).collect();
    println!("keys = {:?}", keys);
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let cases: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(300);
    let seed: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(0xE7F00D);

    if !nvim_driver::nvim_available() {
        eprintln!("nvim not installed — nothing to diff against");
        std::process::exit(2);
    }

    let mut rng = Rng(seed | 1);
    let mut found = 0usize;
    // Dedupe on the (keys, field) pair so one root cause is reported once.
    let mut seen: Vec<String> = Vec::new();

    for i in 0..cases {
        let buffer = gen_buffer(&mut rng);
        let rows = buffer.split('\n').count();
        let row = rng.below(rows);
        let line_len = buffer.split('\n').nth(row).map_or(0, str::len);
        let col = if line_len == 0 {
            0
        } else {
            rng.below(line_len)
        };
        let cmds = gen_commands(&mut rng);

        let case = make_case(&format!("exfuzz_{i}"), &buffer, (row, col));
        let verdict = judge(&case, &cmds).await;
        if !diverges(&verdict) {
            if i % 50 == 0 {
                eprintln!("  [{i}/{cases}] {found} divergence(s)");
            }
            continue;
        }

        let keys: String = cmds.iter().map(|c| format!(":{c}<CR>")).collect();
        let sig = match &verdict {
            Verdict::Diverge { field, .. } => format!("{field}|{keys}"),
            _ => format!("panic|{keys}"),
        };
        if seen.contains(&sig) {
            continue;
        }
        seen.push(sig);

        found += 1;
        report(&case, &cmds, &verdict);
    }

    println!("\n═════════════════════════════════════════════════════════════");
    println!("{cases} cases, {found} distinct divergence(s)");
}
