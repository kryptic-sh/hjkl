//! Randomised differential fuzzer: hjkl vs neovim.
//!
//! Generates random ASCII buffers and random normal-mode keystroke sequences,
//! replays each through both engines, and reports every divergence. Found
//! cases are greedily shrunk (drop one token at a time while the divergence
//! survives) and printed as ready-to-paste corpus TOML.
//!
//! Usage:
//!   cargo run -p hjkl-compat-oracle --release --example difffuzz -- [CASES] [SEED]
//!
//! Deliberate exclusions (harness limits, not engine bugs):
//!   - `:` ex commands — the in-process hjkl driver can't dispatch them
//!     (see `examples/exfuzz.rs` for ex-command fuzzing).
//!   - non-ASCII — hjkl reports char-cols, nvim byte-cols; only equal on ASCII.
//!
//! Search (`/` / `?` / `n` / `N`), folds (`zf` / `zc` / `zo` / `zR` / `zM`)
//! and reflow (`gq`) ARE enabled: the vim FSM resolves the search prompt
//! in-process, the engine applies fold ops to the in-tree view itself, and
//! `textwidth=20` is pinned per-case whenever the generated tokens contain a
//! `gq`. The nvim driver clears its undo history after seeding, so `u` /
//! `<C-r>` diff against empty undo trees on both sides.

use hjkl_compat_oracle::{OracleCase, hjkl_driver, nvim_driver};

// ── tiny deterministic PRNG (no dep) ────────────────────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
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

// ── buffer generation ───────────────────────────────────────────────────────

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
        // Blank lines matter for `{` / `}` / `dap`.
        if rng.chance(6) {
            lines.push(String::new());
        } else {
            lines.push(gen_line(rng));
        }
    }
    lines.join("\n")
}

// ── keystroke generation ────────────────────────────────────────────────────

const MOTIONS: &[&str] = &[
    "h", "j", "k", "l", "w", "W", "b", "B", "e", "E", "ge", "gE", "0", "^", "$", "gg", "G", "{",
    "}", "(", ")", "%", "+", "-", "_", "H", "M", "L", "fo", "to", "Fo", "To", "f)", "t\"", ";",
    ",",
];

const TEXT_OBJECTS: &[&str] = &[
    "iw", "aw", "iW", "aW", "ip", "ap", "i(", "a(", "i{", "a{", "i[", "a[", "i\"", "a\"", "i'",
    "a'", "is", "as",
];

const OPERATORS: &[&str] = &["d", "c", "y", ">", "<", "gu", "gU", "g~", "="];

const SIMPLE_EDITS: &[&str] = &[
    "x", "X", "D", "C", "Y", "J", "gJ", "p", "P", "~", "u", "<C-r>", ".", "s", "S",
];

const INSERT_ENTRIES: &[&str] = &["i", "a", "I", "A", "o", "O"];

const INSERT_TEXT: &[&str] = &["Z", "hi", " ", "q1", "ab cd"];

/// One self-contained keystroke atom. Returned already in vim macro notation.
fn gen_token(rng: &mut Rng) -> String {
    let count = if rng.chance(4) {
        (1 + rng.below(3)).to_string()
    } else {
        String::new()
    };

    match rng.below(13) {
        0..=2 => format!("{count}{}", rng.pick(MOTIONS)),
        3..=4 => {
            let op = rng.pick(OPERATORS);
            match rng.below(3) {
                // Doubled operator (linewise): dd, cc, yy, >>, guu, ...
                0 => {
                    let doubled = match *op {
                        "gu" | "gU" | "g~" => format!("{op}u"),
                        o => format!("{o}{o}"),
                    };
                    format!("{count}{doubled}")
                }
                1 => format!("{count}{op}{}", rng.pick(TEXT_OBJECTS)),
                _ => format!("{count}{op}{}", rng.pick(MOTIONS)),
            }
        }
        5..=6 => format!("{count}{}", rng.pick(SIMPLE_EDITS)),
        7 => format!("{}{}<Esc>", rng.pick(INSERT_ENTRIES), rng.pick(INSERT_TEXT)),
        8 => format!("{count}r{}", rng.pick(&["Z", "x", " ", ")"])),
        9 => {
            // Search: `/pat<CR>` / `?pat<CR>` jump to a match, `n` / `N`
            // repeat it. Plain self-contained tokens, no count — a count on
            // a search would need engine support we don't want to assume.
            match rng.below(4) {
                0 => format!("/{}<CR>", rng.pick(WORDS)),
                1 => format!("?{}<CR>", rng.pick(WORDS)),
                _ => rng.pick(&["n", "N"]).to_string(),
            }
        }
        10 => {
            // Folds (foldmethod=manual, pinned in `make_case`): `zf{motion}`
            // creates a fold — needed for `zc`/`zo`/`zR`/`zM` to mean
            // anything — then the plain fold ops. The engine applies fold
            // ops to the in-tree view itself, so the in-process driver needs
            // no host machinery.
            if rng.chance(2) {
                format!("zf{}", rng.pick(&["j", "G"]))
            } else {
                rng.pick(&["zc", "zo", "zR", "zM"]).to_string()
            }
        }
        11 => rng.pick(&["gqq", "gqj", "gq}"]).to_string(),
        _ => {
            // Visual: enter, move, act.
            let enter = rng.pick(&["v", "V", "<C-v>"]);
            let target = if rng.chance(2) {
                rng.pick(TEXT_OBJECTS).to_string()
            } else {
                format!("{count}{}", rng.pick(MOTIONS))
            };
            let act = rng.pick(&["d", "c<Esc>", "y", "x", ">", "<", "u", "U", "~", "J", "p"]);
            format!("{enter}{target}{act}")
        }
    }
}

fn gen_keys(rng: &mut Rng) -> Vec<String> {
    let n = 1 + rng.below(5);
    (0..n).map(|_| gen_token(rng)).collect()
}

// ── case running ────────────────────────────────────────────────────────────

fn make_case(name: &str, buffer: &str, cursor: (usize, usize), tokens: &[String]) -> OracleCase {
    OracleCase {
        name: name.to_string(),
        initial_buffer: buffer.to_string(),
        initial_cursor: cursor,
        // Always land back in normal mode so the comparison is well-defined.
        keys: format!("{}<Esc>", tokens.concat()),
        expected_buffer: String::new(), // unused: we diff hjkl against nvim directly
        expected_cursor: None,
        expected_mode: None,
        expected_register: None,
        // Pin every setting where hjkl and `nvim --clean` disagree by default,
        // so a divergence means an engine bug rather than config skew.
        shiftwidth: Some(4),
        expandtab: Some(true),
        // textwidth is pinned (20) only for cases that actually reflow (`gq`):
        // nvim auto-wraps insert-mode typing past textwidth while hjkl does
        // not, so a global pin would drown every case that inserts past col 20
        // in insert-wrap divergence noise.
        textwidth: if tokens.iter().any(|t| t.starts_with("gq")) {
            Some(20)
        } else {
            None
        },
        autoindent: Some(false),
        foldmethod: Some("manual".to_string()),
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
    /// nvim itself errored (bad key sequence for its parser, RPC hiccup) —
    /// not a signal about hjkl.
    NvimError,
    HjklPanic,
}

async fn judge(case: &OracleCase) -> Verdict {
    // The in-process driver can panic; a panic IS a finding, so catch it.
    let hjkl = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hjkl_driver::run_case(case)
    })) {
        Ok(Ok(o)) => o,
        Ok(Err(_)) => return Verdict::HjklPanic,
        Err(_) => return Verdict::HjklPanic,
    };

    let Ok(nvim) = nvim_driver::run_case(case).await else {
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

/// Greedily drop tokens (then trim the buffer) while the divergence survives.
async fn shrink(
    buffer: &str,
    cursor: (usize, usize),
    tokens: &[String],
) -> (String, (usize, usize), Vec<String>) {
    let mut tokens = tokens.to_vec();
    let mut buffer = buffer.to_string();
    let mut cursor = cursor;

    // 1. Drop keystroke tokens.
    let mut i = 0;
    while i < tokens.len() {
        let mut trial = tokens.clone();
        trial.remove(i);
        if trial.is_empty() {
            break;
        }
        if diverges(&judge(&make_case("shrink", &buffer, cursor, &trial)).await) {
            tokens = trial;
        } else {
            i += 1;
        }
    }

    // 2. Drop buffer lines (keep the cursor in range).
    loop {
        let lines: Vec<&str> = buffer.split('\n').collect();
        if lines.len() < 2 {
            break;
        }
        let mut shrunk = false;
        for drop in 0..lines.len() {
            let mut trial: Vec<&str> = lines.clone();
            trial.remove(drop);
            let text = trial.join("\n");
            let row = cursor.0.min(trial.len().saturating_sub(1));
            let col = cursor.1.min(trial[row].len().saturating_sub(1));
            if diverges(&judge(&make_case("shrink", &text, (row, col), &tokens)).await) {
                buffer = text;
                cursor = (row, col);
                shrunk = true;
                break;
            }
        }
        if !shrunk {
            break;
        }
    }

    (buffer, cursor, tokens)
}

fn report(case: &OracleCase, verdict: &Verdict) {
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
    println!("keys = {:?}", case.keys);
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let cases: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(500);
    let seed: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(0xC0FFEE);

    if !nvim_driver::nvim_available() {
        eprintln!("nvim not installed — nothing to diff against");
        std::process::exit(2);
    }

    let mut rng = Rng(seed | 1);
    let mut found = 0usize;
    // Dedupe on the shrunk (keys, field) pair so one root cause is reported once.
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
        let tokens = gen_keys(&mut rng);

        let case = make_case(&format!("fuzz_{i}"), &buffer, (row, col), &tokens);
        let verdict = judge(&case).await;
        if !diverges(&verdict) {
            if i % 50 == 0 {
                eprintln!("  [{i}/{cases}] {found} divergence(s)");
            }
            continue;
        }

        let (sbuf, scur, stok) = shrink(&buffer, (row, col), &tokens).await;
        let shrunk = make_case(&format!("fuzz_{i}_shrunk"), &sbuf, scur, &stok);
        let sverdict = judge(&shrunk).await;
        let final_case = if diverges(&sverdict) { shrunk } else { case };
        let final_verdict = if diverges(&sverdict) {
            sverdict
        } else {
            verdict
        };

        let sig = match &final_verdict {
            Verdict::Diverge { field, .. } => format!("{field}|{}", final_case.keys),
            _ => format!("panic|{}", final_case.keys),
        };
        if seen.contains(&sig) {
            continue;
        }
        seen.push(sig);

        found += 1;
        report(&final_case, &final_verdict);
    }

    println!("\n═════════════════════════════════════════════════════════════");
    println!("{cases} cases, {found} distinct divergence(s)");
}
