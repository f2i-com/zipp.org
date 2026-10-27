//! The `tsconfig.json` settings that change what a TypeScript program does at
//! run time: whether class fields are *defined* on the instance
//! (`useDefineForClassFields`, the ES2022 semantics) or *assigned* in the
//! constructor (tsc's output for older targets, and its default), and
//! whether decorators are the legacy ones (`experimentalDecorators`, with
//! `emitDecoratorMetadata`).
//!
//! A file's settings come from the nearest `tsconfig.json` above it, with its
//! `extends` chain applied, as tsc finds them. The file is JSON with comments
//! and trailing commas.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What a TypeScript file's project asks of its run-time semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TsConfig {
    /// `useDefineForClassFields`: class fields are defined on each instance
    /// as the class is constructed (ES2022) rather than assigned at the head
    /// of the constructor.
    pub(crate) define_fields: bool,
    /// `experimentalDecorators`: decorators are TypeScript's legacy ones.
    pub(crate) legacy_decorators: bool,
    /// `emitDecoratorMetadata`: legacy decorators also record types.
    pub(crate) decorator_metadata: bool,
}

impl Default for TsConfig {
    /// tsc's defaults with no `tsconfig.json`: target ES5, so fields are
    /// assigned; standard decorators.
    fn default() -> TsConfig {
        TsConfig {
            define_fields: false,
            legacy_decorators: false,
            decorator_metadata: false,
        }
    }
}

/// The settings for the TypeScript file at `file`.
pub(crate) fn config_for(file: &Path) -> TsConfig {
    static CACHE: Mutex<Vec<(PathBuf, TsConfig)>> = Mutex::new(Vec::new());
    let dir = match file.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if let Ok(cache) = CACHE.lock() {
        if let Some((_, c)) = cache.iter().find(|(d, _)| *d == dir) {
            return *c;
        }
    }
    let config = find_config(&dir)
        .and_then(|path| options_of(&path, 0))
        .map(|o| resolve(&o))
        .unwrap_or_default();
    if let Ok(mut cache) = CACHE.lock() {
        cache.push((dir, config));
    }
    config
}

/// The nearest `tsconfig.json` in `dir` or above it.
fn find_config(dir: &Path) -> Option<PathBuf> {
    let mut d = std::path::absolute(dir).ok()?;
    loop {
        let candidate = d.join("tsconfig.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// The compiler options that matter here, as a config file and its
/// `extends` chain set them (`None`: unset).
#[derive(Default, Clone)]
struct Options {
    target: Option<String>,
    use_define: Option<bool>,
    experimental_decorators: Option<bool>,
    decorator_metadata: Option<bool>,
}

fn resolve(o: &Options) -> TsConfig {
    let define_fields = o.use_define.unwrap_or_else(|| {
        // tsc: `useDefineForClassFields` defaults on for ES2022 and later.
        let t = o.target.as_deref().unwrap_or("es5").to_ascii_lowercase();
        match t.strip_prefix("es") {
            Some("next") => true,
            Some(year) => year.parse::<u32>().is_ok_and(|y| y >= 2022),
            None => false,
        }
    });
    let legacy_decorators = o.experimental_decorators.unwrap_or(false);
    TsConfig {
        define_fields,
        legacy_decorators,
        decorator_metadata: legacy_decorators && o.decorator_metadata.unwrap_or(false),
    }
}

/// The options of the config file at `path`, its bases' first.
fn options_of(path: &Path, depth: u32) -> Option<Options> {
    if depth > 16 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let value = Jsonc::new(&text).parse().ok()?;
    let mut out = Options::default();
    if let Some(ext) = value.get("extends") {
        let bases: Vec<&str> = match ext {
            Value::Str(s) => vec![s.as_str()],
            Value::Arr(items) => items.iter().filter_map(|v| v.as_str()).collect(),
            _ => Vec::new(),
        };
        for base in bases {
            if let Some(file) = locate_base(path, base) {
                if let Some(o) = options_of(&file, depth + 1) {
                    out.target = o.target.or(out.target);
                    out.use_define = o.use_define.or(out.use_define);
                    out.experimental_decorators =
                        o.experimental_decorators.or(out.experimental_decorators);
                    out.decorator_metadata = o.decorator_metadata.or(out.decorator_metadata);
                }
            }
        }
    }
    if let Some(opts) = value.get("compilerOptions") {
        if let Some(t) = opts.get("target").and_then(Value::as_str) {
            out.target = Some(t.to_owned());
        }
        if let Some(Value::Bool(b)) = opts.get("useDefineForClassFields") {
            out.use_define = Some(*b);
        }
        if let Some(Value::Bool(b)) = opts.get("experimentalDecorators") {
            out.experimental_decorators = Some(*b);
        }
        if let Some(Value::Bool(b)) = opts.get("emitDecoratorMetadata") {
            out.decorator_metadata = Some(*b);
        }
    }
    Some(out)
}

/// The file an `extends` entry names: a path relative to the config, or a
/// package's config found in a `node_modules` above it.
fn locate_base(from: &Path, spec: &str) -> Option<PathBuf> {
    let dir = from.parent()?;
    let with_json = |p: PathBuf| -> Option<PathBuf> {
        if p.is_file() {
            return Some(p);
        }
        let mut j = p.clone().into_os_string();
        j.push(".json");
        let j = PathBuf::from(j);
        if j.is_file() {
            return Some(j);
        }
        let t = p.join("tsconfig.json");
        t.is_file().then_some(t)
    };
    if spec.starts_with('.') || Path::new(spec).is_absolute() {
        return with_json(dir.join(spec));
    }
    let mut d = dir.to_path_buf();
    loop {
        if let Some(p) = with_json(d.join("node_modules").join(spec)) {
            return Some(p);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// JSON with comments and trailing commas (a `tsconfig.json`, a
/// `package.json`), or `None` when it is not that.
pub(crate) fn parse_json(text: &str) -> Option<Value> {
    Jsonc::new(text).parse().ok()
}

/// A JSON value, as much as a config needs.
pub(crate) enum Value {
    Null,
    Bool(bool),
    Num,
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(members) => members.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// JSON with `//` and `/* */` comments and trailing commas.
struct Jsonc<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Jsonc<'a> {
    fn new(text: &'a str) -> Jsonc<'a> {
        let b = text.as_bytes();
        let i = if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };
        Jsonc { b, i }
    }

    fn parse(&mut self) -> Result<Value, ()> {
        self.value(0)
    }

    fn skip(&mut self) {
        loop {
            match self.b.get(self.i) {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.i += 1,
                Some(b'/') if self.b.get(self.i + 1) == Some(&b'/') => {
                    while self.i < self.b.len() && self.b[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                Some(b'/') if self.b.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    while self.i + 1 < self.b.len()
                        && !(self.b[self.i] == b'*' && self.b[self.i + 1] == b'/')
                    {
                        self.i += 1;
                    }
                    self.i += 2;
                }
                _ => return,
            }
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value, ()> {
        if depth > 64 {
            return Err(());
        }
        self.skip();
        match self.b.get(self.i).copied().ok_or(())? {
            b'{' => {
                self.i += 1;
                let mut members = Vec::new();
                loop {
                    self.skip();
                    if self.b.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(Value::Obj(members));
                    }
                    let Value::Str(k) = self.value(depth + 1)? else {
                        return Err(());
                    };
                    self.skip();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(());
                    }
                    self.i += 1;
                    let v = self.value(depth + 1)?;
                    members.push((k, v));
                    self.skip();
                    if self.b.get(self.i) == Some(&b',') {
                        self.i += 1;
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Value::Arr(items));
                    }
                    items.push(self.value(depth + 1)?);
                    self.skip();
                    if self.b.get(self.i) == Some(&b',') {
                        self.i += 1;
                    }
                }
            }
            b'"' => {
                self.i += 1;
                let mut s = Vec::new();
                while let Some(&c) = self.b.get(self.i) {
                    self.i += 1;
                    match c {
                        b'"' => return String::from_utf8(s).map(Value::Str).map_err(|_| ()),
                        b'\\' => {
                            let e = *self.b.get(self.i).ok_or(())?;
                            self.i += 1;
                            s.push(match e {
                                b'n' => b'\n',
                                b't' => b'\t',
                                b'r' => b'\r',
                                b'u' => {
                                    // Not needed by any option read here.
                                    self.i += 4;
                                    b'?'
                                }
                                other => other,
                            });
                        }
                        _ => s.push(c),
                    }
                }
                Err(())
            }
            b't' if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            b'f' if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            b'n' if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Value::Null)
            }
            b'-' | b'0'..=b'9' => {
                while matches!(
                    self.b.get(self.i),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.i += 1;
                }
                Ok(Value::Num)
            }
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_with_comments_and_trailing_commas() {
        let v = Jsonc::new("{ // c\n \"compilerOptions\": { /* x */ \"target\": \"ES2022\", \"strict\": true, }, }")
            .parse()
            .unwrap_or(Value::Null);
        let t = v
            .get("compilerOptions")
            .and_then(|o| o.get("target"))
            .and_then(Value::as_str);
        assert_eq!(t, Some("ES2022"));
    }

    #[test]
    fn targets_decide_the_default() {
        let at = |t: &str| {
            resolve(&Options {
                target: Some(t.into()),
                ..Options::default()
            })
            .define_fields
        };
        assert!(at("ES2022") && at("esnext") && at("ES2024"));
        assert!(!at("es2021") && !at("ES5") && !at("es2015"));
        assert!(!resolve(&Options::default()).define_fields);
        assert!(
            resolve(&Options {
                target: Some("es5".into()),
                use_define: Some(true),
                ..Options::default()
            })
            .define_fields
        );
    }
}
