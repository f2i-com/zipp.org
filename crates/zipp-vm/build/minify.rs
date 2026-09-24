// Comment stripping for the Python library and runtime sources the Python
// frontend embeds (`src/frontend/python/{lib,runtime}`). Shared by
// `build.rs`, which writes the stripped copies to `$OUT_DIR/pysrc/`, and by
// the frontend's `minify_check` test, which proves the stripped text
// compiles to the same program.
//
// The one rule is that every token of code keeps its exact line AND column:
// tracebacks through bundled modules print `file, line`, compile errors
// print `line:col`, and PEP 563 annotations slice the source between two
// token offsets. So nothing is ever moved, only removed from the END of a
// line:
//
//   * a comment is cut to the end of its line (a whole-line comment leaves
//     an empty line, never a deleted one);
//   * trailing spaces/tabs are trimmed from a line that ends OUTSIDE any
//     string/template literal (inside one they are content);
//   * a JavaScript block comment keeps every line break it contained (a
//     multi-line one is a LineTerminator for ASI); text after its `*/` on the
//     same line keeps its column through space padding.
//
// Strings, docstrings, template literals and regular-expression literals are
// copied byte for byte; `__doc__` and every literal value are unchanged.

/// Python: `#` comments outside string literals.
#[allow(dead_code)]
pub fn strip_python(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                trim_line_end(&mut out);
            }
            b'\n' => {
                trim_line_end(&mut out);
                out.push(b'\n');
                i += 1;
            }
            b'\'' | b'"' => {
                let triple = i + 2 < b.len() && b[i + 1] == c && b[i + 2] == c;
                let start = i;
                if triple {
                    i += 3;
                    loop {
                        if i >= b.len() {
                            break;
                        }
                        if b[i] == b'\\' {
                            i += 2;
                            continue;
                        }
                        if b[i] == c && i + 2 < b.len() && b[i + 1] == c && b[i + 2] == c {
                            i += 3;
                            break;
                        }
                        i += 1;
                    }
                } else {
                    i += 1;
                    while i < b.len() {
                        if b[i] == b'\\' {
                            i += 2;
                            continue;
                        }
                        if b[i] == c || b[i] == b'\n' {
                            if b[i] == c {
                                i += 1;
                            }
                            break;
                        }
                        i += 1;
                    }
                }
                let end = i.min(b.len());
                out.extend_from_slice(&b[start..end]);
                i = end;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("comment stripping keeps UTF-8 boundaries")
}

/// JavaScript: `//` and `/* */` comments outside string, template and
/// regular-expression literals.
#[allow(dead_code)]
pub fn strip_javascript(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    // One entry per open `${` of a template literal: the `{` depth at which
    // its matching `}` resumes the template.
    let mut templates: Vec<usize> = Vec::new();
    let mut depth = 0usize;
    // Whether a `/` here would begin a regular expression (no expression
    // just ended) rather than divide.
    let mut regex_ok = true;
    // One entry per open `(`: whether it follows `if`/`while`/`for`/`with`,
    // whose `)` ends a head rather than an operand (`if (x) /re/.test(s)`).
    let mut parens: Vec<bool> = Vec::new();
    let mut after_control = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // Only whitespace and comments may sit between a control keyword and
        // its `(`; anything else closes that window (a word re-opens it).
        let comment = c == b'/' && matches!(b.get(i + 1), Some(b'/' | b'*'));
        if !c.is_ascii_whitespace() && c != b'(' && !comment {
            after_control = false;
        }
        match c {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                trim_line_end(&mut out);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut j = i + 2;
                while j + 1 < b.len() && !(b[j] == b'*' && b[j + 1] == b'/') {
                    j += 1;
                }
                let end = (j + 2).min(b.len());
                let body = &b[i..end];
                let breaks = body.iter().filter(|&&x| x == b'\n').count();
                // Columns (in bytes) of the text after `*/`: on a later line,
                // the width of the comment's last line; on the same line, the
                // width of the comment itself.
                let after = &b[end..];
                let rest_of_line = after.iter().position(|&x| x == b'\n').map_or(after, |p| &after[..p]);
                let code_follows = rest_of_line.iter().any(|&x| x != b' ' && x != b'\t');
                if breaks == 0 {
                    if code_follows {
                        out.extend(std::iter::repeat_n(b' ', body.len()));
                    } else {
                        trim_line_end(&mut out);
                    }
                } else {
                    trim_line_end(&mut out);
                    for _ in 0..breaks {
                        out.push(b'\n');
                    }
                    if code_follows {
                        let last = body.iter().rposition(|&x| x == b'\n').unwrap();
                        out.extend(std::iter::repeat_n(b' ', body.len() - last - 1));
                    }
                }
                i = end;
                // A comment is whitespace: `regex_ok` is unchanged.
            }
            b'\n' => {
                trim_line_end(&mut out);
                out.push(b'\n');
                i += 1;
            }
            b'\'' | b'"' => {
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == c || b[i] == b'\n' {
                        if b[i] == c {
                            i += 1;
                        }
                        break;
                    }
                    i += 1;
                }
                let end = i.min(b.len());
                out.extend_from_slice(&b[start..end]);
                i = end;
                regex_ok = false;
            }
            b'`' => {
                let open = templates.len();
                i = copy_template(b, i + 1, &mut out, &mut templates, depth);
                // Stopped at a `${`: an expression starts; else one ended.
                regex_ok = templates.len() > open;
            }
            b'}' if templates.last() == Some(&depth) => {
                // The `}` closing a `${ ... }` substitution: the template
                // literal continues.
                templates.pop();
                let open = templates.len();
                i = copy_template(b, i + 1, &mut out, &mut templates, depth);
                regex_ok = templates.len() > open;
            }
            b'/' if regex_ok => {
                let start = i;
                i += 1;
                let mut class = false;
                while i < b.len() {
                    match b[i] {
                        b'\\' => {
                            i += 2;
                            continue;
                        }
                        b'[' => class = true,
                        b']' => class = false,
                        b'/' if !class => {
                            i += 1;
                            break;
                        }
                        b'\n' => break,
                        _ => {}
                    }
                    i += 1;
                }
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                    i += 1;
                }
                let end = i.min(b.len());
                out.extend_from_slice(&b[start..end]);
                i = end;
                regex_ok = false;
            }
            _ if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80) {
                    i += 1;
                }
                let word = &b[start..i];
                out.extend_from_slice(word);
                after_control = matches!(word, b"if" | b"while" | b"for" | b"with");
                regex_ok = matches!(
                    word,
                    b"return" | b"typeof" | b"case" | b"do" | b"else" | b"in" | b"of" | b"new" | b"delete"
                        | b"void" | b"throw" | b"instanceof" | b"yield" | b"await"
                );
            }
            b' ' | b'\t' | b'\r' => {
                out.push(c);
                i += 1;
            }
            _ => {
                let mut head_closed = false;
                match c {
                    b'{' => depth += 1,
                    b'}' => depth = depth.saturating_sub(1),
                    b'(' => parens.push(std::mem::take(&mut after_control)),
                    b')' => head_closed = parens.pop().unwrap_or(false),
                    _ => {}
                }
                // After `)` / `]` / `}` an operand has ended, so `/` divides,
                // except after the `)` of a control-statement head. (`}`
                // closing a block could precede a regex; the runtime never
                // starts a statement with one, and a misread shows up as a
                // difference `minify_check` reports.)
                regex_ok = head_closed || !matches!(c, b')' | b']' | b'}');
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("comment stripping keeps UTF-8 boundaries")
}

/// Copy a template literal's text from `i` (just after its opening backtick,
/// or after the `}` ending a substitution) up to and including its closing
/// backtick or the next `${`. Returns the index after what was copied.
fn copy_template(b: &[u8], mut i: usize, out: &mut Vec<u8>, templates: &mut Vec<usize>, depth: usize) -> usize {
    // From the opening backtick, or the `}` that resumes the template.
    let start = i - 1;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'`' => {
                i += 1;
                break;
            }
            b'$' if b.get(i + 1) == Some(&b'{') => {
                i += 2;
                templates.push(depth);
                break;
            }
            _ => i += 1,
        }
    }
    let end = i.min(b.len());
    out.extend_from_slice(&b[start..end]);
    end
}

/// Remove the spaces and tabs at the end of the output's current line.
fn trim_line_end(out: &mut Vec<u8>) {
    while matches!(out.last(), Some(b' ' | b'\t')) {
        out.pop();
    }
}
