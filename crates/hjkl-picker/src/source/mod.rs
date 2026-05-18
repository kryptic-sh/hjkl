pub mod file;
pub mod rg;

pub use file::FileSource;
pub use rg::{
    GrepBackend, RgMatch, RgSource, detect_grep_backend, extract_json_string, extract_json_u32,
    parse_grep_line, parse_rg_json_line,
};
