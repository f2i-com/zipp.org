//! Decimal text to `f64`: the engine's one string-to-number conversion.
//!
//! The converter is `crates/zipp-numparse`: [`parse_f64`] and
//! [`parse_f64_bytes`] return, for every input, exactly what
//! `s.parse::<f64>().ok()` returns, without linking core's 10 KB
//! power-of-five table (see that crate for how, and for its tests). It is a
//! crate of its own so that the Python front end, which cannot depend on the
//! engine, parses its float literals with it too. Numeric literals,
//! `Number(str)`, `parseFloat`, `JSON.parse` and the rest call it through
//! this module; outside the engine it is `zipp_vm::embed::parse_f64`.
//!
//! The test below keeps a stray `parse::<f64>()` from linking core's table
//! back in.

pub(crate) use zipp_numparse::{parse_f64, parse_f64_bytes};

#[cfg(test)]
mod tests {
    /// Nothing in the engine, its WebAssembly embedding or the Python front
    /// end may parse a float with core: one call links `dec2flt` and its
    /// table back into every module that has that crate, and the bytes
    /// `zipp-numparse` saves are gone with no test failing. This reads the
    /// three crates' sources for the spellings such a call takes, including
    /// an untyped `.parse()` on a line that names an `f64` constant
    /// (`text.parse().unwrap_or(f64::NAN)` in a function returning `f64`,
    /// as the Python front end once read its float literals). A `.parse()`
    /// whose `f64` is inferred some other way gets past it; the full check
    /// is `twiggy top` on a built module, searched for `dec2flt`.
    #[test]
    fn no_core_float_parse_in_engine_sources() {
        fn visit(dir: &std::path::Path, found: &mut Vec<String>) {
            let entries =
                std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            for entry in entries {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    visit(&path, found);
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.ends_with(".rs") || name == "num_parse.rs" {
                    continue;
                }
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                for (i, line) in text.lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with("//") {
                        continue;
                    }
                    let parses = |ty: &str| {
                        code.contains(&format!("parse::<{ty}>"))
                            || code.contains(&format!("{ty}::from_str"))
                            || (code.contains(".parse()")
                                && (code.contains(&format!(": {ty} = "))
                                    || code.contains(&format!("{ty}::"))))
                    };
                    if parses("f64") || parses("f32") {
                        found.push(format!("{}:{}: {code}", path.display(), i + 1));
                    }
                }
            }
        }
        let vm = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        visit(&vm.join("src"), &mut found);
        visit(&vm.join("../zipp-wasm/src"), &mut found);
        visit(&vm.join("../zipp-pyparse/src"), &mut found);
        assert!(
            found.is_empty(),
            "core's float parser links a 10 KB table into every WebAssembly module; use \
             `crate::num_parse::parse_f64` (`zipp_vm::embed::parse_f64` outside the engine, \
             `zipp_numparse::parse_f64` in the Python front end):\n{}",
            found.join("\n")
        );
    }
}
