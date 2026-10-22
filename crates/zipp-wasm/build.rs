//! Writes `src/preamble.js` without its comment lines and blank lines, for
//! `lib.rs` to embed. The preamble is prepended to every script the engine
//! compiles, and half of it is commentary the lexer would otherwise skip on
//! every `initScript` and every module would otherwise carry.
//!
//! Dropping whole lines is sound while no template literal and no string
//! spans lines, which is asserted below: a line that starts with `//` is
//! then a comment and nothing else. Guest positions do not depend on the
//! preamble's length (they count from the guest's first line); a frame of
//! the preamble's code reports its line in this embedded text.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/preamble.js");
    println!("cargo:rerun-if-changed=build.rs");
    let source = fs::read_to_string("src/preamble.js").expect("read src/preamble.js");
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        assert!(
            line.matches('`').count() % 2 == 0 && !line.trim_end().ends_with('\\'),
            "src/preamble.js: a template literal or string spans lines at {line:?}"
        );
        let text = line.trim_start();
        if text.is_empty() || text.starts_with("//") {
            continue;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    let dest = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set for build scripts"));
    fs::write(dest.join("preamble.js"), out).expect("write the embedded preamble");
}
