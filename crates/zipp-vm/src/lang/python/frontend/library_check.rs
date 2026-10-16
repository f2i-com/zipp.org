//! The bundled library table (`lib/modules.txt`, built into `BUNDLED_MODULES`
//! and, with the torch package built in, `TORCH_MODULES` by build.rs): every
//! module's import statements, read at build time, name exactly the modules
//! the frontend's own `imported_modules` finds in the parsed module, so
//! following a project's imports through a library module that is not
//! compiled reaches what compiling it would have. The torch package build
//! (crates/zipp-wasm/torch) records its modules' imports with the same
//! scanner, so this covers them too.
//!
//!   cargo test -p zipp-vm --features python --lib library_check
use super::{bundled_imports, imported_modules, parse_module, Bundled, BUNDLED_MODULES};
use std::collections::BTreeSet;

fn tables() -> Vec<&'static Bundled> {
    #[allow(unused_mut)]
    let mut all: Vec<&'static Bundled> = BUNDLED_MODULES.iter().collect();
    #[cfg(not(feature = "python-no-torch"))]
    all.extend(super::TORCH_MODULES.iter());
    all
}

#[test]
fn library_check_import_lists_match_the_parser() {
    for module in tables() {
        let bump = zipp_pyparse::tree::Bump::new();
        let suite = parse_module(module.file, module.source, &bump).unwrap_or_else(|e| panic!("{}: {e}", module.name));
        let mut parsed = BTreeSet::new();
        imported_modules(&suite, module.name, &mut parsed);
        let mut listed = BTreeSet::new();
        bundled_imports(module.imports, module.name, &mut listed);
        assert_eq!(parsed, listed, "{} ({})", module.name, module.file);
    }
}

#[test]
fn library_check_names_are_unique_and_valid() {
    let mut seen = BTreeSet::new();
    for module in BUNDLED_MODULES {
        assert_eq!(module.package, "base", "{}", module.name);
    }
    #[cfg(not(feature = "python-no-torch"))]
    for module in super::TORCH_MODULES {
        assert_eq!(module.package, "torch", "{}", module.name);
        assert!(module.name == "torch" || module.name.starts_with("torch."), "{}", module.name);
    }
    for module in tables() {
        assert!(super::is_module_name(module.name), "{}", module.name);
        assert!(seen.insert(module.name), "duplicate {}", module.name);
    }
}
