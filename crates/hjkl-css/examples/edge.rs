fn main() {
    let dc_cases: &[(&str, &str)] = &[
        ("descendant type", "label span { color: red; }"),
        ("descendant class right", "label .b { color: red; }"),
        ("descendant pseudo right", "label :hover { color: red; }"),
        ("class then type", ".a label { color: red; }"),
        ("pseudo then type", ":hover label { color: red; }"),
    ];
    for (label, css) in dc_cases {
        match hjkl_css::parse(css) {
            Ok(s) => {
                println!("OK   [{label}] -> {} rules", s.rules.len());
                for r in &s.rules {
                    println!("       sels={:?}", r.selectors);
                }
            }
            Err(e) => println!("ERR  [{label}] -> {e}"),
        }
    }
    println!("---");
    let cases = [
        ("empty rule", ".foo { }"),
        ("multi decl semi", ".foo { color: red; padding: 10px; }"),
        ("trailing no semi", ".foo { color: red; padding: 10px }"),
        ("comment", "/* x */ .foo { color: red; } /* y */"),
        ("@-rule charset", "@charset \"utf-8\"; .foo { color: red; }"),
        (
            "@media unsupported",
            "@media (min-width: 100px) { .foo { color: red; } }",
        ),
        ("!important", ".foo { color: red !important; }"),
        ("font shorthand unsupported", ".foo { font: 12px Arial; }"),
        ("uppercase pseudo", ".foo:HOVER { color: red; }"),
        ("ID hash starting digit", ".foo { color: #1a2b3c; }"),
        ("rgb with spaces", ".foo { color: rgb( 255 , 0 , 0 ); }"),
        ("triple class", "a.b.c { color: red; }"),
        ("class then pseudo", ".foo:hover { color: red; }"),
        ("pseudo then class", ":hover.foo { color: red; }"),
    ];
    for (label, css) in cases {
        match hjkl_css::parse(css) {
            Ok(s) => println!("OK   [{label}] -> {} rules", s.rules.len()),
            Err(e) => println!("ERR  [{label}] -> {e}"),
        }
    }
}
