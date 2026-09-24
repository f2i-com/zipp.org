//! The bundled library table (`lib/modules.txt`, built into `BUNDLED_MODULES`
//! by build.rs): every module's import statements, read at build time, name
//! exactly the modules the frontend's own `imported_modules` finds in the
//! parsed module, so following a project's imports through a library module
//! that is not compiled reaches what compiling it would have.
//!
//!   cargo test -p zipp-vm --features python --lib library_check
use super::{bundled_imports, imported_modules, parse_module, BUNDLED_MODULES};
use std::collections::BTreeSet;

#[test]
fn library_check_import_lists_match_the_parser() {
    for module in BUNDLED_MODULES {
        let suite = parse_module(module.file, module.source).unwrap_or_else(|e| panic!("{}: {e}", module.name));
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
        assert!(super::is_module_name(module.name), "{}", module.name);
        assert!(seen.insert(module.name), "duplicate {}", module.name);
        assert!(matches!(module.package, "base" | "torch"), "{}: package {}", module.name, module.package);
    }
}
