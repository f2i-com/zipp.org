// Generates `$OUT_DIR/instr_codec.rs`: the binary encoder and decoder of every
// `bytecode::Instr` variant (and of the fieldless enums its fields use), read
// from src/bytecode.rs itself so the codec can never fall behind the
// instruction set. `src/bytecode_codec.rs` includes it. A field type the
// generator does not know stops the build with a message naming it.
use std::fmt::Write;

/// Fieldless enums an `Instr` field may have, encoded as their variant index.
const SMALL_ENUMS: &[&str] = &[
    "MathFn",
    "GlobalFn",
    "RegExpMethod",
    "StaticFn",
    "BitwiseOp",
    "PyArithOp",
    "PyCmpOp",
];

fn strip_comments(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let line = match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        };
        let t = line.trim();
        if t.starts_with("#[cfg") {
            panic!("instr_codec: a `#[cfg]` inside an encoded enum is not supported: {t}");
        }
        if t.starts_with("#[") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The text between the braces of `pub enum <name> {`.
fn enum_body<'a>(src: &'a str, name: &str) -> &'a str {
    let head = format!("pub enum {name} {{");
    let start = src
        .find(&head)
        .unwrap_or_else(|| panic!("instr_codec: no `{head}` in bytecode.rs"))
        + head.len();
    let mut depth = 1usize;
    for (i, c) in src[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[start..start + i];
                }
            }
            _ => {}
        }
    }
    panic!("instr_codec: unterminated enum {name}");
}

/// Split at top-level commas (not inside `<>`, `()`, `{}`).
fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '<' | '(' | '{' | '[' => depth += 1,
            '>' | ')' | '}' | ']' => depth -= 1,
            _ => {}
        }
        if c == ',' && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

enum Shape {
    Unit,
    Struct(Vec<(String, String)>),
    Tuple(Vec<String>),
}

fn variants(body: &str) -> Vec<(String, Shape)> {
    split_top(&strip_comments(body))
        .into_iter()
        .map(|v| {
            if let Some(i) = v.find('{') {
                let name = v[..i].trim().to_owned();
                let fields = split_top(&v[i + 1..v.rfind('}').unwrap()])
                    .into_iter()
                    .map(|f| {
                        let (n, t) = f.split_once(':').unwrap_or_else(|| panic!("instr_codec: field {f:?} of {name}"));
                        (n.trim().to_owned(), t.trim().to_owned())
                    })
                    .collect();
                (name, Shape::Struct(fields))
            } else if let Some(i) = v.find('(') {
                let name = v[..i].trim().to_owned();
                let fields = split_top(&v[i + 1..v.rfind(')').unwrap()]);
                (name, Shape::Tuple(fields))
            } else {
                let name = v.split('=').next().unwrap().trim().to_owned();
                (name, Shape::Unit)
            }
        })
        .collect()
}

/// (encode expression for a `&T` binding `x`, decode expression) of a type.
fn codec(ty: &str, context: &str) -> (String, String) {
    let t = ty.replace(' ', "");
    match t.as_str() {
        "Reg" | "u16" => ("w.u16(*x)".into(), "r.u16()?".into()),
        "u32" => ("w.u32(*x)".into(), "r.u32()?".into()),
        "u8" => ("w.u8(*x)".into(), "r.u8()?".into()),
        "bool" => ("w.bool(*x)".into(), "r.bool()?".into()),
        "i32" => ("w.i64(*x as i64)".into(), "r.i64()? as i32".into()),
        "i64" => ("w.i64(*x)".into(), "r.i64()?".into()),
        "i128" => ("w.i128(*x)".into(), "r.i128()?".into()),
        "f64" => ("w.f64(*x)".into(), "r.f64()?".into()),
        "Option<Reg>" | "Option<u16>" => ("w.opt_u16(*x)".into(), "r.opt_u16()?".into()),
        "Option<u32>" => ("w.opt_u32(*x)".into(), "r.opt_u32()?".into()),
        _ if SMALL_ENUMS.contains(&t.as_str()) => (
            format!("w.u8(enc_{t}(*x))"),
            format!("dec_{t}(r.u8()?)?"),
        ),
        _ => panic!(
            "instr_codec: {context} has a field of type `{ty}`, which build/instr_codec.rs cannot encode yet"
        ),
    }
}

pub fn generate(bytecode_rs: &str) -> String {
    // Comments go first, so a brace in a doc comment cannot unbalance the
    // enum bodies.
    let src: String = bytecode_rs
        .replace("\r\n", "\n")
        .lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::new();
    out.push_str("// Generated by build/instr_codec.rs from src/bytecode.rs. Do not edit.\n");
    for name in SMALL_ENUMS {
        let vs = variants(enum_body(&src, name));
        writeln!(out, "#[inline]\n#[allow(non_snake_case)]\nfn enc_{name}(v: {name}) -> u8 {{\n    match v {{").unwrap();
        for (i, (v, shape)) in vs.iter().enumerate() {
            assert!(matches!(shape, Shape::Unit), "instr_codec: {name}::{v} has fields");
            writeln!(out, "        {name}::{v} => {i},").unwrap();
        }
        writeln!(out, "    }}\n}}").unwrap();
        writeln!(out, "#[inline]\n#[allow(non_snake_case)]\nfn dec_{name}(b: u8) -> Result<{name}, String> {{\n    Ok(match b {{").unwrap();
        for (i, (v, _)) in vs.iter().enumerate() {
            writeln!(out, "        {i} => {name}::{v},").unwrap();
        }
        writeln!(out, "        _ => return Err(format!(\"bad {name} {{b}}\")),\n    }})\n}}").unwrap();
    }
    let vs = variants(enum_body(&src, "Instr"));
    writeln!(out, "pub(crate) const INSTR_VARIANTS: usize = {};", vs.len()).unwrap();
    out.push_str("fn encode_instr(w: &mut Writer, i: &Instr) {\n    match i {\n");
    for (tag, (name, shape)) in vs.iter().enumerate() {
        match shape {
            Shape::Unit => writeln!(out, "        Instr::{name} => w.u16({tag}),").unwrap(),
            Shape::Struct(fields) => {
                let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                writeln!(out, "        Instr::{name} {{ {} }} => {{\n            w.u16({tag});", names.join(", ")).unwrap();
                for (n, t) in fields {
                    let (enc, _) = codec(t, &format!("Instr::{name}"));
                    writeln!(out, "            {{ let x = {n}; {enc}; }}").unwrap();
                }
                out.push_str("        }\n");
            }
            Shape::Tuple(types) => {
                let names: Vec<String> = (0..types.len()).map(|k| format!("f{k}")).collect();
                writeln!(out, "        Instr::{name}({}) => {{\n            w.u16({tag});", names.join(", ")).unwrap();
                for (n, t) in names.iter().zip(types) {
                    let (enc, _) = codec(t, &format!("Instr::{name}"));
                    writeln!(out, "            {{ let x = {n}; {enc}; }}").unwrap();
                }
                out.push_str("        }\n");
            }
        }
    }
    out.push_str("    }\n}\n");
    out.push_str("fn decode_instr(r: &mut Reader) -> Result<Instr, String> {\n    let tag = r.u16()?;\n    Ok(match tag {\n");
    for (tag, (name, shape)) in vs.iter().enumerate() {
        match shape {
            Shape::Unit => writeln!(out, "        {tag} => Instr::{name},").unwrap(),
            Shape::Struct(fields) => {
                writeln!(out, "        {tag} => Instr::{name} {{").unwrap();
                for (n, t) in fields {
                    let (_, dec) = codec(t, &format!("Instr::{name}"));
                    writeln!(out, "            {n}: {dec},").unwrap();
                }
                out.push_str("        },\n");
            }
            Shape::Tuple(types) => {
                let decs: Vec<String> = types
                    .iter()
                    .map(|t| codec(t, &format!("Instr::{name}")).1)
                    .collect();
                writeln!(out, "        {tag} => Instr::{name}({}),", decs.join(", ")).unwrap();
            }
        }
    }
    out.push_str("        _ => return Err(format!(\"bad instruction tag {tag}\")),\n    })\n}\n");
    out
}
