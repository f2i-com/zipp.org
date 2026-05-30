//! `JSON.stringify` / `JSON.parse`.

use crate::interp::{EvalResult, Interp};
use crate::value::{JsValue, ObjData, Object};

pub fn stringify(it: &Interp, _this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let v = args.first().cloned().unwrap_or(JsValue::Undefined);
    let indent = match args.get(2) {
        Some(JsValue::Num(n)) if *n > 0.0 => " ".repeat((*n as usize).min(10)),
        Some(JsValue::Str(s)) => s.chars().take(10).collect(),
        _ => String::new(),
    };
    let mut out = String::new();
    if write_value(it, &v, &indent, 0, &mut out)? {
        Ok(JsValue::str(out))
    } else {
        Ok(JsValue::Undefined)
    }
}

/// Returns `false` if the value serializes to "nothing" (undefined / function),
/// so callers can omit object entries (and the top level returns `undefined`).
fn write_value(it: &Interp, v: &JsValue, indent: &str, depth: usize, out: &mut String) -> EvalResult<bool> {
    match v {
        JsValue::Undefined => Ok(false),
        JsValue::Null => {
            out.push_str("null");
            Ok(true)
        }
        JsValue::Bool(b) => {
            out.push_str(if *b { "true" } else { "false" });
            Ok(true)
        }
        JsValue::Num(n) => {
            if n.is_finite() {
                out.push_str(&crate::value::num_to_string(*n));
            } else {
                out.push_str("null"); // NaN / Infinity → null, per spec
            }
            Ok(true)
        }
        JsValue::Str(s) => {
            write_string(s, out);
            Ok(true)
        }
        JsValue::Object(o) => {
            let b = o.borrow();
            match &b.data {
                ObjData::Function { .. } | ObjData::Native { .. } => Ok(false),
                ObjData::Array(items) => {
                    let items = items.clone();
                    drop(b);
                    write_array(it, &items, indent, depth, out)?;
                    Ok(true)
                }
                ObjData::Plain => {
                    let keys = b.order.clone();
                    let pairs: Vec<(String, JsValue)> =
                        keys.into_iter().map(|k| (k.clone(), b.props.get(&k).cloned().unwrap_or(JsValue::Undefined))).collect();
                    drop(b);
                    write_object(it, &pairs, indent, depth, out)?;
                    Ok(true)
                }
            }
        }
    }
}

fn write_array(it: &Interp, items: &[JsValue], indent: &str, depth: usize, out: &mut String) -> EvalResult<()> {
    if items.is_empty() {
        out.push_str("[]");
        return Ok(());
    }
    let pretty = !indent.is_empty();
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        newline_indent(pretty, indent, depth + 1, out);
        // array holes / undefined / functions serialize as null
        let mut tmp = String::new();
        if write_value(it, item, indent, depth + 1, &mut tmp)? {
            out.push_str(&tmp);
        } else {
            out.push_str("null");
        }
    }
    newline_indent(pretty, indent, depth, out);
    out.push(']');
    Ok(())
}

fn write_object(it: &Interp, pairs: &[(String, JsValue)], indent: &str, depth: usize, out: &mut String) -> EvalResult<()> {
    let pretty = !indent.is_empty();
    let mut wrote = false;
    out.push('{');
    for (k, v) in pairs {
        let mut tmp = String::new();
        if !write_value(it, v, indent, depth + 1, &mut tmp)? {
            continue; // omit undefined / function-valued properties
        }
        if wrote {
            out.push(',');
        }
        wrote = true;
        newline_indent(pretty, indent, depth + 1, out);
        write_string(k, out);
        out.push(':');
        if pretty {
            out.push(' ');
        }
        out.push_str(&tmp);
    }
    if wrote {
        newline_indent(pretty, indent, depth, out);
    }
    out.push('}');
    Ok(())
}

fn newline_indent(pretty: bool, indent: &str, depth: usize, out: &mut String) {
    if pretty {
        out.push('\n');
        for _ in 0..depth {
            out.push_str(indent);
        }
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ───────────────────────── parse ─────────────────────────

pub fn parse(it: &Interp, _this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let text = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    let chars: Vec<char> = text.chars().collect();
    let mut p = Parser { c: &chars, i: 0, it };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.c.len() {
        return Err(p.err("Unexpected non-whitespace character after JSON"));
    }
    Ok(v)
}

struct Parser<'a> {
    c: &'a [char],
    i: usize,
    it: &'a Interp,
}

impl Parser<'_> {
    fn err(&self, msg: &str) -> JsValue {
        self.it.make_error("SyntaxError", &format!("{msg} (at position {})", self.i))
    }
    fn ws(&mut self) {
        while self.i < self.c.len() && matches!(self.c[self.i], ' ' | '\t' | '\n' | '\r') {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn value(&mut self) -> EvalResult<JsValue> {
        self.ws();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(JsValue::str(self.string()?)),
            Some('t') => self.literal("true", JsValue::Bool(true)),
            Some('f') => self.literal("false", JsValue::Bool(false)),
            Some('n') => self.literal("null", JsValue::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            _ => Err(self.err("Unexpected token in JSON")),
        }
    }
    fn literal(&mut self, word: &str, v: JsValue) -> EvalResult<JsValue> {
        for ch in word.chars() {
            if self.peek() != Some(ch) {
                return Err(self.err("Unexpected token in JSON"));
            }
            self.i += 1;
        }
        Ok(v)
    }
    fn number(&mut self) -> EvalResult<JsValue> {
        let start = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.peek() == Some('.') {
            self.i += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let s: String = self.c[start..self.i].iter().collect();
        s.parse::<f64>().map(JsValue::Num).map_err(|_| self.err("Invalid number in JSON"))
    }
    fn string(&mut self) -> EvalResult<String> {
        self.i += 1; // opening quote
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err(self.err("Unterminated string in JSON")),
                Some('"') => {
                    self.i += 1;
                    return Ok(s);
                }
                Some('\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some('"') => s.push('"'),
                        Some('\\') => s.push('\\'),
                        Some('/') => s.push('/'),
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('b') => s.push('\x08'),
                        Some('f') => s.push('\x0c'),
                        Some('u') => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                self.i += 1;
                                let d = self.peek().and_then(|c| c.to_digit(16)).ok_or_else(|| self.err("Invalid \\u escape"))?;
                                code = code * 16 + d;
                            }
                            s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("Invalid escape in JSON")),
                    }
                    self.i += 1;
                }
                Some(c) => {
                    s.push(c);
                    self.i += 1;
                }
            }
        }
    }
    fn array(&mut self) -> EvalResult<JsValue> {
        self.i += 1; // [
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(JsValue::Object(Object::array(items)));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(',') => {
                    self.i += 1;
                }
                Some(']') => {
                    self.i += 1;
                    return Ok(JsValue::Object(Object::array(items)));
                }
                _ => return Err(self.err("Expected ',' or ']' in JSON array")),
            }
        }
    }
    fn object(&mut self) -> EvalResult<JsValue> {
        self.i += 1; // {
        let o = Object::plain();
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(JsValue::Object(o));
        }
        loop {
            self.ws();
            if self.peek() != Some('"') {
                return Err(self.err("Expected string key in JSON object"));
            }
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(':') {
                return Err(self.err("Expected ':' in JSON object"));
            }
            self.i += 1;
            let v = self.value()?;
            o.borrow_mut().set(&key, v);
            self.ws();
            match self.peek() {
                Some(',') => {
                    self.i += 1;
                }
                Some('}') => {
                    self.i += 1;
                    return Ok(JsValue::Object(o));
                }
                _ => return Err(self.err("Expected ',' or '}' in JSON object")),
            }
        }
    }
}
