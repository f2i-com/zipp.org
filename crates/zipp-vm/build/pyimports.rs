// The import statements of a bundled Python module, read at build time so a
// program can follow its imports without compiling it (see `Bundled` in
// src/frontend/python/mod.rs). Mirrors what the frontend's
// `imported_modules` collects from the parsed module: every `import` and
// `from ... import` statement anywhere in the module, except inside
// `async def` / `async for` / `async with` bodies. The frontend's
// `library_check` test compares the two on every bundled module.

/// One import statement: `import a.b, c` or `from ..m import x, y`.
#[derive(Debug, PartialEq)]
pub enum ImportStmt {
    Plain(Vec<String>),
    From(u32, Option<String>, Vec<String>),
}

/// The source's logical lines: (indentation, text), strings blanked to `""`
/// and comments dropped, bracketed continuations joined.
fn logical_lines(src: &str) -> Vec<(usize, String)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut line = String::new();
    let mut indent: Option<usize> = None;
    let mut col = 0usize; // leading whitespace of the current physical line
    let mut depth = 0usize;
    let mut i = 0;
    let push = |line: &mut String, indent: &mut Option<usize>, out: &mut Vec<(usize, String)>| {
        let text = line.trim().to_owned();
        if !text.is_empty() {
            out.push((indent.unwrap_or(0), text));
        }
        line.clear();
        *indent = None;
    };
    while i < b.len() {
        let c = b[i];
        if indent.is_none() {
            if c == b' ' || c == b'\t' {
                col += 1;
                i += 1;
                continue;
            }
            if c == b'\n' || c == b'\r' {
                col = 0;
                i += 1;
                continue;
            }
            if c == b'#' {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            indent = Some(col);
        }
        match c {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' if b.get(i + 1) == Some(&b'\n') => {
                line.push(' ');
                i += 2;
            }
            b'\\' if b.get(i + 1) == Some(&b'\r') => {
                line.push(' ');
                i += 3;
            }
            b'\n' => {
                if depth == 0 {
                    push(&mut line, &mut indent, &mut out);
                    col = 0;
                } else {
                    line.push(' ');
                }
                i += 1;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                line.push(c as char);
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                line.push(c as char);
                i += 1;
            }
            b'\'' | b'"' => {
                let triple = i + 2 < b.len() && b[i + 1] == c && b[i + 2] == c;
                i += if triple { 3 } else { 1 };
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if triple {
                        if b[i] == c && i + 2 < b.len() && b[i + 1] == c && b[i + 2] == c {
                            i += 3;
                            break;
                        }
                    } else if b[i] == c || b[i] == b'\n' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                line.push_str("\"\"");
            }
            _ => {
                // Multi-byte UTF-8 passes through byte by byte as '?'; only
                // ASCII syntax matters here.
                line.push(if c.is_ascii() { c as char } else { '?' });
                i += 1;
            }
        }
    }
    push(&mut line, &mut indent, &mut out);
    out
}

/// `text` split at `sep` where no bracket is open.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if c == sep && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn first_word(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    &text[..end]
}

const COMPOUND: &[&str] = &[
    "if", "elif", "else", "for", "while", "with", "try", "except", "finally", "def", "class", "match", "case", "async",
];

fn simple_import(stmt: &str) -> Option<ImportStmt> {
    let stmt = stmt.trim();
    match first_word(stmt) {
        "import" => {
            let names = stmt["import".len()..]
                .split(',')
                .map(|part| part.split_whitespace().next().unwrap_or("").to_owned())
                .filter(|n| !n.is_empty())
                .collect();
            Some(ImportStmt::Plain(names))
        }
        "from" => {
            let rest = stmt["from".len()..].trim_start();
            let at = rest.find(" import").or_else(|| rest.find(")import"))?;
            let (target, names) = (rest[..at].trim(), rest[at..].trim_start_matches(')').trim());
            let names = names.strip_prefix("import")?.trim();
            let names = names.trim_start_matches('(').trim_end_matches(')');
            let level = target.chars().take_while(|&c| c == '.').count() as u32;
            let module = target[level as usize..].trim();
            let module = (!module.is_empty()).then(|| module.to_owned());
            let names = names
                .split(',')
                .map(|part| part.split_whitespace().next().unwrap_or("").to_owned())
                .filter(|n| !n.is_empty())
                .collect();
            Some(ImportStmt::From(level, module, names))
        }
        _ => None,
    }
}

/// Every import statement of `src` the frontend's `imported_modules` sees.
#[allow(dead_code)]
pub fn imports(src: &str) -> Vec<ImportStmt> {
    let mut out = Vec::new();
    // Indentations of the `async` blocks being skipped.
    let mut skipping: Option<usize> = None;
    for (indent, text) in logical_lines(src) {
        if let Some(level) = skipping {
            if indent > level {
                continue;
            }
            skipping = None;
        }
        let word = first_word(&text);
        if COMPOUND.contains(&word) {
            // The header ends at the first colon outside brackets; a body on
            // the same line follows it.
            let parts = split_top(&text, ':');
            if parts.len() < 2 {
                continue; // `match = ...` and friends: not a header
            }
            let body = parts[1..].join(":");
            if word == "async" {
                if body.trim().is_empty() {
                    skipping = Some(indent);
                }
                continue;
            }
            for stmt in split_top(&body, ';') {
                out.extend(simple_import(stmt));
            }
            continue;
        }
        for stmt in split_top(&text, ';') {
            out.extend(simple_import(stmt));
        }
    }
    out
}
