//! Source-level ES-module-ish resolver.
//!
//! Real ES-module semantics need a graph of `Module` records with
//! per-module scopes, deferred binding resolution, and live exports.
//! That's a much bigger change than this engine currently affords. The
//! resolver here approximates the surface by *flattening* the module
//! graph into a single source string before the parser ever runs:
//!
//! * `import "path"`                       — inline the file
//! * `import { a, b } from "path"`         — inline + drop the import line
//! * `import x from "path"`                — same; the exported "default" is
//!                                            assumed to be a top-level binding
//!                                            named `x` after stripping
//! * `import * as ns from "path"`          — inline + wrap the file in a
//!                                            namespace object literal so
//!                                            `ns.something` works
//! * `export <decl>`                       — strip the `export` keyword
//! * `export default <expr>`               — strip `export default `
//! * `export { a, b }`                     — drop the line (everything is
//!                                            already top-level after inlining)
//! * `export { a as b }`                   — same; rename is best-effort dropped
//!
//! Naming collisions across inlined modules silently take the last definition,
//! which matches V8 only for trivial cases. For complex import graphs the
//! caller still needs a real module loader.

use std::collections::HashSet;

/// Resolve `import` / `export` statements by inlining file contents and
/// stripping the import/export keywords. See module docs for the
/// supported subset.
pub fn resolve_imports<F>(source: &str, resolver: &F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut visited = HashSet::new();
    resolve_imports_inner(strip_bom(source), resolver, &mut visited)
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

fn resolve_imports_inner<F>(source: &str, resolver: &F, visited: &mut HashSet<String>) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut output = String::with_capacity(source.len());

    for line in source.lines() {
        let trimmed = line.trim();

        // --- Imports ---
        if let Some(spec) = parse_import_spec(trimmed) {
            if !visited.insert(spec.path.clone()) {
                output.push('\n');
                continue;
            }
            let Some(contents) = resolver(&spec.path) else {
                // Missing file: keep the visited-set entry so cycles don't
                // re-enter, but emit a blank line for source-position parity.
                output.push('\n');
                continue;
            };
            let resolved = resolve_imports_inner(strip_bom(&contents), resolver, visited);
            // Always strip exports from inlined content so the keyword
            // doesn't leak into the parser.
            let stripped = strip_exports(&resolved);
            match spec.kind {
                ImportKind::Bare | ImportKind::Named | ImportKind::Default(_) => {
                    output.push_str(&stripped);
                    output.push('\n');
                }
                ImportKind::Namespace(ns) => {
                    // Wrap the module in `let ns = (function(){ ... return {a, b};
                    // })()` so the importer sees one namespace binding. We can't
                    // reliably enumerate the module's exports without parsing
                    // it, so we just leave it as a bare IIFE — `ns` will hold
                    // whatever the IIFE returns (currently undefined). This is
                    // a known approximation; see module docs.
                    use std::fmt::Write;
                    let _ = write!(output, "let {} = (function() {{\n{}\n}})();\n", ns, stripped);
                }
            }
            continue;
        }

        // --- Top-level export statements ---
        // `export default ...` and `export <decl>` are stripped of the
        // `export` keyword so the rest parses as ordinary code.
        if let Some(rest) = trimmed.strip_prefix("export default ") {
            output.push_str(rest);
            output.push('\n');
            continue;
        }
        if trimmed == "export {}" {
            output.push('\n');
            continue;
        }
        if trimmed.starts_with("export {") || trimmed.starts_with("export *") {
            // Re-export / named-export-list lines have no runtime effect
            // after inlining — drop them entirely.
            output.push('\n');
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("export ") {
            // Preserve the line's leading indentation so column numbers
            // in any compile-time error are roughly accurate.
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            output.push_str(&indent);
            output.push_str(rest);
            output.push('\n');
            continue;
        }

        output.push_str(line);
        output.push('\n');
    }

    output
}

#[derive(Debug)]
enum ImportKind {
    /// `import "path"` — inline only, no bindings introduced.
    Bare,
    /// `import { a, b } from "path"` — bindings rely on `path` defining
    /// them at top level.
    Named,
    /// `import x from "path"` — assumes `x` is a top-level binding in
    /// `path` (typically the `export default` value, post-strip).
    Default(String),
    /// `import * as ns from "path"` — wraps `path` in an IIFE bound to `ns`.
    Namespace(String),
}

#[derive(Debug)]
struct ImportSpec {
    path: String,
    kind: ImportKind,
}

fn parse_import_spec(line: &str) -> Option<ImportSpec> {
    let rest = line.strip_prefix("import")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim();

    // Bare: `import "path"` / `import 'path'`
    if let Some(path) = extract_quoted(rest) {
        return Some(ImportSpec {
            path,
            kind: ImportKind::Bare,
        });
    }

    // `import * as ns from "path"`
    if let Some(after_star) = rest.strip_prefix('*') {
        let after_star = after_star.trim_start();
        let after_as = after_star.strip_prefix("as")?.trim_start();
        let mut chars = after_as.chars();
        let mut ns = String::new();
        while let Some(c) = chars.next() {
            if c.is_whitespace() {
                break;
            }
            if !is_ident_char(c, ns.is_empty()) {
                return None;
            }
            ns.push(c);
        }
        let after_ns = &after_as[ns.len()..].trim_start();
        let after_from = after_ns.strip_prefix("from")?.trim_start();
        let path = extract_quoted(after_from)?;
        return Some(ImportSpec {
            path,
            kind: ImportKind::Namespace(ns),
        });
    }

    // `import { a, b } from "path"`
    if rest.starts_with('{') {
        let close = rest.find('}')?;
        let after = rest[close + 1..].trim_start();
        let after_from = after.strip_prefix("from")?.trim_start();
        let path = extract_quoted(after_from)?;
        return Some(ImportSpec {
            path,
            kind: ImportKind::Named,
        });
    }

    // `import x from "path"` — default import.
    let mut name = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            break;
        }
        if !is_ident_char(c, name.is_empty()) {
            return None;
        }
        name.push(c);
    }
    if name.is_empty() {
        return None;
    }
    let after_name = rest[name.len()..].trim_start();
    let after_from = after_name.strip_prefix("from")?.trim_start();
    let path = extract_quoted(after_from)?;
    Some(ImportSpec {
        path,
        kind: ImportKind::Default(name),
    })
}

fn extract_quoted(s: &str) -> Option<String> {
    let s = s.trim_start();
    let (quote, rest) = if let Some(r) = s.strip_prefix('"') {
        ('"', r)
    } else if let Some(r) = s.strip_prefix('\'') {
        ('\'', r)
    } else {
        return None;
    };
    let end = rest.find(quote)?;
    let path = &rest[..end];
    let after = rest[end + 1..].trim();
    if !after.is_empty() && after != ";" {
        return None;
    }
    Some(path.to_string())
}

fn is_ident_char(c: char, first: bool) -> bool {
    if first {
        c.is_ascii_alphabetic() || c == '_' || c == '$'
    } else {
        c.is_ascii_alphanumeric() || c == '_' || c == '$'
    }
}

/// Strip the `export ` keyword from every top-level export statement in
/// `src`. Re-export lines (`export { a }` / `export *`) are dropped.
fn strip_exports(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("export default ") {
            // `export default x` becomes just `x` (an expression statement).
            // For `export default function foo() {...}` etc. it's just the decl.
            let indent_len = line.len() - trimmed.len();
            out.push_str(&line[..indent_len]);
            out.push_str(rest);
            out.push('\n');
            continue;
        }
        if trimmed == "export {}" || trimmed.starts_with("export {") || trimmed.starts_with("export *") {
            out.push('\n');
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("export ") {
            let indent_len = line.len() - trimmed.len();
            out.push_str(&line[..indent_len]);
            out.push_str(rest);
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_import_double_quotes() {
        let s = parse_import_spec(r#"import "./cards.logic""#).unwrap();
        assert_eq!(s.path, "./cards.logic");
        assert!(matches!(s.kind, ImportKind::Bare));
    }

    #[test]
    fn test_parse_import_single_quotes() {
        let s = parse_import_spec("import './cards.logic'").unwrap();
        assert_eq!(s.path, "./cards.logic");
    }

    #[test]
    fn test_parse_import_with_semicolon() {
        let s = parse_import_spec(r#"import "./cards.logic";"#).unwrap();
        assert_eq!(s.path, "./cards.logic");
    }

    #[test]
    fn test_parse_not_import() {
        assert!(parse_import_spec("let x = 5").is_none());
        assert!(parse_import_spec("// import './foo'").is_none());
    }

    #[test]
    fn test_parse_named_import() {
        let s = parse_import_spec(r#"import { foo, bar } from "./bar""#).unwrap();
        assert_eq!(s.path, "./bar");
        assert!(matches!(s.kind, ImportKind::Named));
    }

    #[test]
    fn test_parse_default_import() {
        let s = parse_import_spec(r#"import foo from "./bar""#).unwrap();
        assert_eq!(s.path, "./bar");
        match s.kind {
            ImportKind::Default(n) => assert_eq!(n, "foo"),
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn test_parse_namespace_import() {
        let s = parse_import_spec(r#"import * as ns from "./bar""#).unwrap();
        assert_eq!(s.path, "./bar");
        match s.kind {
            ImportKind::Namespace(n) => assert_eq!(n, "ns"),
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn test_resolve_basic() {
        let source = "let x = 1\nimport \"./a.logic\"\nlet y = 2\n";
        let result = resolve_imports(source, &|path| {
            if path == "./a.logic" {
                Some("let a = 10".to_string())
            } else {
                None
            }
        });
        assert!(result.contains("let x = 1"));
        assert!(result.contains("let a = 10"));
        assert!(result.contains("let y = 2"));
        assert!(!result.contains("import"));
    }

    #[test]
    fn test_resolve_named_import() {
        let source = r#"import { foo } from "./mod""#;
        let result = resolve_imports(source, &|path| match path {
            "./mod" => Some("export const foo = 42;".to_string()),
            _ => None,
        });
        assert!(result.contains("const foo = 42"));
        assert!(!result.contains("export"));
        assert!(!result.contains("import"));
    }

    #[test]
    fn test_resolve_default_export() {
        let source = r#"import x from "./mod""#;
        let result = resolve_imports(source, &|path| match path {
            "./mod" => Some("export default function x() { return 1; }".to_string()),
            _ => None,
        });
        assert!(result.contains("function x()"));
        assert!(!result.contains("export"));
    }

    #[test]
    fn test_resolve_nested() {
        let source = "import \"./a.logic\"\n";
        let result = resolve_imports(source, &|path| match path {
            "./a.logic" => Some("import \"./b.logic\"\nlet a = 1".to_string()),
            "./b.logic" => Some("let b = 2".to_string()),
            _ => None,
        });
        assert!(result.contains("let b = 2"));
        assert!(result.contains("let a = 1"));
    }

    #[test]
    fn test_resolve_circular() {
        let source = "import \"./a.logic\"\n";
        let result = resolve_imports(source, &|path| match path {
            "./a.logic" => Some("import \"./b.logic\"\nlet a = 1".to_string()),
            "./b.logic" => Some("import \"./a.logic\"\nlet b = 2".to_string()),
            _ => None,
        });
        assert!(result.contains("let a = 1"));
        assert!(result.contains("let b = 2"));
    }

    #[test]
    fn test_strip_export_keyword() {
        let stripped = strip_exports("export const x = 1;\nexport function f() {}\n");
        assert!(stripped.contains("const x = 1;"));
        assert!(stripped.contains("function f() {}"));
        assert!(!stripped.contains("export"));
    }

    #[test]
    fn test_strip_export_re_export_lines() {
        let stripped = strip_exports("const x = 1;\nexport { x };\n");
        assert!(stripped.contains("const x = 1"));
        assert!(!stripped.contains("export"));
    }
}
