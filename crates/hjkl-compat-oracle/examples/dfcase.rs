//! Replay a single ad-hoc case through both engines and print both outcomes.
//!
//! Usage:
//!   cargo run -p hjkl-compat-oracle --release --example dfcase -- \
//!       '<buffer with \n escapes>' <row> <col> '<keys>' [textwidth]
//!
//! `textwidth` is unset unless given — pass `20` to replay a `difffuzz` case
//! that reflows, which pins it to that.
//!
//! Companion to `difffuzz`: that finds divergences, this one lets you poke at
//! a single case by hand while narrowing down the cause.

use hjkl_compat_oracle::{OracleCase, hjkl_driver, nvim_driver};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        eprintln!("usage: dfcase '<buffer>' <row> <col> '<keys>' [textwidth]");
        std::process::exit(2);
    }
    let buffer = a[0].replace("\\t", "\t").replace("\\n", "\n");
    let case = OracleCase {
        name: "adhoc".into(),
        initial_buffer: buffer,
        initial_cursor: (a[1].parse().unwrap(), a[2].parse().unwrap()),
        keys: a[3].clone(),
        expected_buffer: String::new(),
        expected_cursor: None,
        expected_mode: None,
        expected_register: None,
        shiftwidth: Some(4),
        expandtab: Some(true),
        textwidth: a
            .get(4)
            .map(|tw| tw.parse().expect("textwidth must be a number")),
        tabstop: None,
        autoindent: Some(false),
        foldmethod: Some("manual".into()),
    };

    println!(
        "initial = {:?} @ {:?}",
        case.initial_buffer, case.initial_cursor
    );
    println!("keys    = {:?}\n", case.keys);

    match hjkl_driver::run_case(&case) {
        Ok(o) => println!(
            "hjkl: buf={:?}\n      cur={:?} mode={} reg={:?}",
            o.buffer, o.cursor, o.mode, o.default_register
        ),
        Err(e) => println!("hjkl: ERROR {e}"),
    }
    match nvim_driver::run_case(&case).await {
        Ok(o) => println!(
            "nvim: buf={:?}\n      cur={:?} mode={} reg={:?}",
            o.buffer, o.cursor, o.mode, o.default_register
        ),
        Err(e) => println!("nvim: ERROR {e}"),
    }
}
