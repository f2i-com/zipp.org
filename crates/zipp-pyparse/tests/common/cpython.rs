//! Checks against CPython's own parser: where CPython accepts a source,
//! ZIPP must accept it and build the same tree. The tree is compared as JSON: this dumps the arena AST in the shape
//! `cpython_check.py` dumps CPython's `ast` (positions left out; number
//! literals as their text, which the script evaluates).

#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;
use zipp_pyparse::ast::*;

/// The Python to run, if any: `PYPARSE_PYTHON`, else `py -3.13`, `python3`.
fn python() -> Option<(String, Vec<String>)> {
    if let Ok(cmd) = std::env::var("PYPARSE_PYTHON") {
        return Some((cmd, Vec::new()));
    }
    for (cmd, args) in [
        ("py", vec!["-3.13"]),
        ("python3", vec![]),
        ("python", vec![]),
    ] {
        let ok = Command::new(cmd)
            .args(&args)
            .args(["-c", "import sys; assert sys.version_info >= (3, 12)"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some((cmd.to_owned(), args.iter().map(|s| s.to_string()).collect()));
        }
    }
    None
}

/// A command running the found CPython, if any.
pub fn python_command() -> Option<Command> {
    let (cmd, args) = python()?;
    let mut command = Command::new(cmd);
    command.args(args);
    Some(command)
}

/// CPython's standard library directory.
pub fn stdlib() -> Option<PathBuf> {
    let (cmd, args) = python()?;
    let output = Command::new(cmd)
        .args(args)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_paths()['stdlib'])",
        ])
        .output()
        .ok()?;
    let dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    dir.is_dir().then_some(dir)
}

/// Check `(label, source)` pairs; `None` without a CPython (3.12+).
pub fn check(sources: &[(String, String)]) -> Option<Vec<String>> {
    run_check(sources, None)
}

/// Check only that CPython accepts each source.
pub fn check_accepts(sources: &[(String, String)]) -> Option<Vec<String>> {
    run_check(sources, Some("null"))
}

/// Where CPython accepts a source, ours must accept it with the same tree.
pub fn check_if_valid(sources: &[(String, String)]) -> Option<Vec<String>> {
    let tagged: Vec<(String, String)> = sources
        .iter()
        .map(|(label, source)| (format!("{label} [if valid]"), source.clone()))
        .collect();
    run_check(&tagged, None)
}

/// Where CPython accepts a source, ours must accept it with the same tree;
/// what only ours accepts is not checked.
pub fn check_where_cpython_accepts(sources: &[(String, String)]) -> Option<Vec<String>> {
    let tagged: Vec<(String, String)> = sources
        .iter()
        .map(|(label, source)| (format!("{label} [where CPython accepts]"), source.clone()))
        .collect();
    run_check(&tagged, None)
}

/// Check that CPython rejects each source.
pub fn check_rejects(sources: &[(String, String)]) -> Option<Vec<String>> {
    run_check(sources, Some("\"reject\""))
}

fn run_check(sources: &[(String, String)], expect: Option<&str>) -> Option<Vec<String>> {
    if sources.is_empty() {
        return Some(Vec::new());
    }
    let (cmd, args) = python()?;
    let mut input = String::new();
    for (label, source) in sources {
        let ours = if let Some(expect) = expect {
            expect.to_owned()
        } else {
            match zipp_pyparse::parse(
                source,
                zipp_pyparse::Mode::Module,
                0,
                zipp_pyparse::Limits::NONE,
            ) {
                Ok(module) => dump_module(&module),
                Err(e) => format!("{{\"error\": {}}}", json_str(&e.message)),
            }
        };
        writeln!(
            input,
            "{{\"label\": {}, \"source\": {}, \"ours\": {ours}}}",
            json_str(label),
            json_str(source)
        )
        .unwrap();
    }
    // One directory per call: tests run concurrently in one process.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("zipp-pyparse-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("cases.jsonl");
    std::fs::write(&path, input).ok()?;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/common/cpython_check.py");
    let output = Command::new(cmd)
        .args(args)
        .arg(script)
        .arg(&path)
        .output()
        .ok()?;
    let _ = std::fs::remove_dir_all(&dir);
    if !output.status.success() {
        return Some(vec![format!(
            "cpython_check.py failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )]);
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // ASCII only, so no character can split the line when Python
            // reads the file back.
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    write!(out, "\\u{unit:04x}").unwrap();
                }
            }
        }
    }
    out.push('"');
    out
}

struct Dumper<'m, 's> {
    m: &'m Module<'s>,
    out: String,
}

pub fn dump_module(m: &Module) -> String {
    let mut d = Dumper {
        m,
        out: String::new(),
    };
    d.out.push_str("[\"Module\", {\"body\": ");
    d.stmts(m.body);
    d.out.push_str(", \"type_ignores\": []}]");
    d.out
}

impl Dumper<'_, '_> {
    fn node(&mut self, name: &str, fields: &mut dyn FnMut(&mut Self)) {
        write!(self.out, "[\"{name}\", {{").unwrap();
        fields(self);
        self.out.push_str("}]");
    }
    fn field(&mut self, name: &str, first: bool) {
        if !first {
            self.out.push_str(", ");
        }
        write!(self.out, "\"{name}\": ").unwrap();
    }
    fn sym(&mut self, s: zipp_pyparse::intern::Sym) {
        let name = self.m.name(s).to_owned();
        self.out.push_str(&json_str(&name));
    }
    fn opt_sym(&mut self, s: Option<zipp_pyparse::intern::Sym>) {
        match s {
            Some(s) => self.sym(s),
            None => self.out.push_str("null"),
        }
    }
    fn stmts(&mut self, list: List<StmtId>) {
        self.out.push('[');
        for (i, &s) in self.m.list(list).iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.stmt(s);
        }
        self.out.push(']');
    }
    fn exprs(&mut self, list: List<ExprId>) {
        self.out.push('[');
        for (i, &e) in self.m.list(list).iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.expr(e);
        }
        self.out.push(']');
    }
    fn opt_expr(&mut self, e: Option<ExprId>) {
        match e {
            Some(e) => self.expr(e),
            None => self.out.push_str("null"),
        }
    }
    fn ctx(&mut self, c: ExprContext) {
        let name = match c {
            ExprContext::Load => "Load",
            ExprContext::Store => "Store",
            ExprContext::Del => "Del",
        };
        write!(self.out, "[\"{name}\", {{}}]").unwrap();
    }
    fn op(&mut self, name: &str) {
        write!(self.out, "[\"{name}\", {{}}]").unwrap();
    }
    fn operator(&mut self, op: Operator) {
        self.op(&format!("{op:?}"));
    }

    fn expr(&mut self, id: ExprId) {
        let m = self.m;
        let e = *m.expr(id);
        match e.kind {
            ExprKind::BoolOp { op, values } => self.node("BoolOp", &mut |d| {
                d.field("op", true);
                d.op(&format!("{op:?}"));
                d.field("values", false);
                d.exprs(values);
            }),
            ExprKind::NamedExpr { target, value } => self.node("NamedExpr", &mut |d| {
                d.field("target", true);
                d.expr(target);
                d.field("value", false);
                d.expr(value);
            }),
            ExprKind::BinOp { left, op, right } => self.node("BinOp", &mut |d| {
                d.field("left", true);
                d.expr(left);
                d.field("op", false);
                d.operator(op);
                d.field("right", false);
                d.expr(right);
            }),
            ExprKind::UnaryOp { op, operand } => self.node("UnaryOp", &mut |d| {
                d.field("op", true);
                d.op(&format!("{op:?}"));
                d.field("operand", false);
                d.expr(operand);
            }),
            ExprKind::Lambda { args, body } => self.node("Lambda", &mut |d| {
                d.field("args", true);
                d.arguments(args);
                d.field("body", false);
                d.expr(body);
            }),
            ExprKind::IfExp { test, body, orelse } => self.node("IfExp", &mut |d| {
                d.field("test", true);
                d.expr(test);
                d.field("body", false);
                d.expr(body);
                d.field("orelse", false);
                d.expr(orelse);
            }),
            ExprKind::Dict { keys, values } => self.node("Dict", &mut |d| {
                d.field("keys", true);
                d.out.push('[');
                for (i, k) in d.m.list(keys).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    d.opt_expr(*k);
                }
                d.out.push(']');
                d.field("values", false);
                d.exprs(values);
            }),
            ExprKind::Set { elts } => self.node("Set", &mut |d| {
                d.field("elts", true);
                d.exprs(elts);
            }),
            ExprKind::ListComp { elt, generators } => self.comp("ListComp", elt, None, generators),
            ExprKind::SetComp { elt, generators } => self.comp("SetComp", elt, None, generators),
            ExprKind::GeneratorExp { elt, generators } => {
                self.comp("GeneratorExp", elt, None, generators)
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => self.comp("DictComp", key, Some(value), generators),
            ExprKind::Await { value } => self.node("Await", &mut |d| {
                d.field("value", true);
                d.expr(value);
            }),
            ExprKind::Yield { value } => self.node("Yield", &mut |d| {
                d.field("value", true);
                d.opt_expr(value);
            }),
            ExprKind::YieldFrom { value } => self.node("YieldFrom", &mut |d| {
                d.field("value", true);
                d.expr(value);
            }),
            ExprKind::Compare {
                left,
                ops,
                comparators,
            } => self.node("Compare", &mut |d| {
                d.field("left", true);
                d.expr(left);
                d.field("ops", false);
                d.out.push('[');
                for (i, op) in d.m.list(ops).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    d.op(&format!("{op:?}"));
                }
                d.out.push(']');
                d.field("comparators", false);
                d.exprs(comparators);
            }),
            ExprKind::Call {
                func,
                args,
                keywords,
            } => self.node("Call", &mut |d| {
                d.field("func", true);
                d.expr(func);
                d.field("args", false);
                d.exprs(args);
                d.field("keywords", false);
                d.out.push('[');
                for (i, k) in d.m.list(keywords).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    let k = *k;
                    d.node("keyword", &mut |d| {
                        d.field("arg", true);
                        d.opt_sym(k.arg);
                        d.field("value", false);
                        d.expr(k.value);
                    });
                }
                d.out.push(']');
            }),
            ExprKind::FormattedValue {
                value,
                conversion,
                format_spec,
            } => self.node("FormattedValue", &mut |d| {
                d.field("value", true);
                d.expr(value);
                d.field("conversion", false);
                let c = match conversion {
                    Conversion::None => -1,
                    Conversion::Str => 115,
                    Conversion::Repr => 114,
                    Conversion::Ascii => 97,
                };
                write!(d.out, "{c}").unwrap();
                d.field("format_spec", false);
                d.opt_expr(format_spec);
            }),
            ExprKind::JoinedStr { values } => self.node("JoinedStr", &mut |d| {
                d.field("values", true);
                d.exprs(values);
            }),
            ExprKind::Constant(c) => self.node("Constant", &mut |d| {
                d.field("value", true);
                match c {
                    Constant::None => d.out.push_str("null"),
                    Constant::True => d.out.push_str("true"),
                    Constant::False => d.out.push_str("false"),
                    Constant::Ellipsis => d.out.push_str("{\"ellipsis\": 1}"),
                    Constant::Int { radix, text } => {
                        let text = d.m.number_text(text);
                        write!(
                            d.out,
                            "{{\"int_text\": {}, \"radix\": {radix}}}",
                            json_str(text)
                        )
                        .unwrap();
                    }
                    Constant::Float { text } => {
                        let text = d.m.number_text(text);
                        write!(d.out, "{{\"float_text\": {}}}", json_str(text)).unwrap();
                    }
                    Constant::Complex { text } => {
                        let text = d.m.number_text(text);
                        write!(d.out, "{{\"complex_text\": {}}}", json_str(text)).unwrap();
                    }
                    Constant::Str { value, .. } => {
                        let s = d.m.str(value).to_owned();
                        d.out.push_str(&json_str(&s));
                    }
                    Constant::Bytes { value } => {
                        let s: String = d.m.bytes(value).iter().map(|&b| b as char).collect();
                        write!(d.out, "{{\"bytes\": {}}}", json_str(&s)).unwrap();
                    }
                }
            }),
            ExprKind::Attribute { value, attr, ctx } => self.node("Attribute", &mut |d| {
                d.field("value", true);
                d.expr(value);
                d.field("attr", false);
                d.sym(attr);
                d.field("ctx", false);
                d.ctx(ctx);
            }),
            ExprKind::Subscript { value, slice, ctx } => self.node("Subscript", &mut |d| {
                d.field("value", true);
                d.expr(value);
                d.field("slice", false);
                d.expr(slice);
                d.field("ctx", false);
                d.ctx(ctx);
            }),
            ExprKind::Starred { value, ctx } => self.node("Starred", &mut |d| {
                d.field("value", true);
                d.expr(value);
                d.field("ctx", false);
                d.ctx(ctx);
            }),
            ExprKind::Name { id, ctx } => self.node("Name", &mut |d| {
                d.field("id", true);
                d.sym(id);
                d.field("ctx", false);
                d.ctx(ctx);
            }),
            ExprKind::List { elts, ctx } => self.node("List", &mut |d| {
                d.field("elts", true);
                d.exprs(elts);
                d.field("ctx", false);
                d.ctx(ctx);
            }),
            ExprKind::Tuple { elts, ctx } => self.node("Tuple", &mut |d| {
                d.field("elts", true);
                d.exprs(elts);
                d.field("ctx", false);
                d.ctx(ctx);
            }),
            ExprKind::Slice { lower, upper, step } => self.node("Slice", &mut |d| {
                d.field("lower", true);
                d.opt_expr(lower);
                d.field("upper", false);
                d.opt_expr(upper);
                d.field("step", false);
                d.opt_expr(step);
            }),
        }
    }

    fn comp(
        &mut self,
        name: &str,
        elt: ExprId,
        value: Option<ExprId>,
        generators: List<Comprehension>,
    ) {
        self.node(name, &mut |d| {
            if let Some(value) = value {
                d.field("key", true);
                d.expr(elt);
                d.field("value", false);
                d.expr(value);
            } else {
                d.field("elt", true);
                d.expr(elt);
            }
            d.field("generators", false);
            d.out.push('[');
            for (i, c) in d.m.list(generators).iter().enumerate() {
                if i > 0 {
                    d.out.push_str(", ");
                }
                let c = *c;
                d.node("comprehension", &mut |d| {
                    d.field("target", true);
                    d.expr(c.target);
                    d.field("iter", false);
                    d.expr(c.iter);
                    d.field("ifs", false);
                    d.exprs(c.ifs);
                    d.field("is_async", false);
                    write!(d.out, "{}", c.is_async as u8).unwrap();
                });
            }
            d.out.push(']');
        });
    }

    fn param(&mut self, p: &Param) {
        let p = *p;
        self.node("arg", &mut |d| {
            d.field("arg", true);
            d.sym(p.name);
            d.field("annotation", false);
            d.opt_expr(p.annotation);
        });
    }

    fn arguments(&mut self, id: ArgsId) {
        let m = self.m;
        let a = *m.arguments(id);
        self.node("arguments", &mut |d| {
            let params = |d: &mut Self, list: List<Param>| {
                d.out.push('[');
                for (i, p) in m.list(list).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    d.param(p);
                }
                d.out.push(']');
            };
            d.field("posonlyargs", true);
            params(d, a.posonlyargs);
            d.field("args", false);
            params(d, a.args);
            d.field("vararg", false);
            match a.vararg {
                Some(i) => d.param(&m.params[i as usize]),
                None => d.out.push_str("null"),
            }
            d.field("kwonlyargs", false);
            params(d, a.kwonlyargs);
            d.field("kw_defaults", false);
            d.out.push('[');
            for (i, p) in m.list(a.kwonlyargs).iter().enumerate() {
                if i > 0 {
                    d.out.push_str(", ");
                }
                d.opt_expr(p.default);
            }
            d.out.push(']');
            d.field("kwarg", false);
            match a.kwarg {
                Some(i) => d.param(&m.params[i as usize]),
                None => d.out.push_str("null"),
            }
            d.field("defaults", false);
            d.out.push('[');
            let mut first = true;
            for p in m.list(a.posonlyargs).iter().chain(m.list(a.args)) {
                if let Some(e) = p.default {
                    if !first {
                        d.out.push_str(", ");
                    }
                    first = false;
                    d.expr(e);
                }
            }
            d.out.push(']');
        });
    }

    fn type_params(&mut self, list: List<TypeParam>) {
        self.out.push('[');
        for (i, p) in self.m.list(list).iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            let p = *p;
            let name = match p.kind {
                TypeParamKind::TypeVar { .. } => "TypeVar",
                TypeParamKind::ParamSpec => "ParamSpec",
                TypeParamKind::TypeVarTuple => "TypeVarTuple",
            };
            self.node(name, &mut |d| {
                d.field("name", true);
                d.sym(p.name);
                if let TypeParamKind::TypeVar { bound } = p.kind {
                    d.field("bound", false);
                    d.opt_expr(bound);
                }
                d.field("default_value", false);
                d.opt_expr(p.default);
            });
        }
        self.out.push(']');
    }

    fn body_fields(&mut self, name: &str, list: List<StmtId>, first: bool) {
        self.field(name, first);
        self.stmts(list);
    }

    fn stmt(&mut self, id: StmtId) {
        let m = self.m;
        let s = *m.stmt(id);
        match s.kind {
            StmtKind::FunctionDef {
                is_async,
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
            } => self.node(
                if is_async {
                    "AsyncFunctionDef"
                } else {
                    "FunctionDef"
                },
                &mut |d| {
                    d.field("name", true);
                    d.sym(name);
                    d.field("args", false);
                    d.arguments(args);
                    d.body_fields("body", body, false);
                    d.field("decorator_list", false);
                    d.exprs(decorator_list);
                    d.field("returns", false);
                    d.opt_expr(returns);
                    d.field("type_params", false);
                    d.type_params(type_params);
                },
            ),
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            } => self.node("ClassDef", &mut |d| {
                d.field("name", true);
                d.sym(name);
                d.field("bases", false);
                d.exprs(bases);
                d.field("keywords", false);
                d.out.push('[');
                for (i, k) in m.list(keywords).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    let k = *k;
                    d.node("keyword", &mut |d| {
                        d.field("arg", true);
                        d.opt_sym(k.arg);
                        d.field("value", false);
                        d.expr(k.value);
                    });
                }
                d.out.push(']');
                d.body_fields("body", body, false);
                d.field("decorator_list", false);
                d.exprs(decorator_list);
                d.field("type_params", false);
                d.type_params(type_params);
            }),
            StmtKind::Return { value } => self.node("Return", &mut |d| {
                d.field("value", true);
                d.opt_expr(value);
            }),
            StmtKind::Delete { targets } => self.node("Delete", &mut |d| {
                d.field("targets", true);
                d.exprs(targets);
            }),
            StmtKind::Assign { targets, value } => self.node("Assign", &mut |d| {
                d.field("targets", true);
                d.exprs(targets);
                d.field("value", false);
                d.expr(value);
            }),
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => self.node("TypeAlias", &mut |d| {
                d.field("name", true);
                d.expr(name);
                d.field("type_params", false);
                d.type_params(type_params);
                d.field("value", false);
                d.expr(value);
            }),
            StmtKind::AugAssign { target, op, value } => self.node("AugAssign", &mut |d| {
                d.field("target", true);
                d.expr(target);
                d.field("op", false);
                d.operator(op);
                d.field("value", false);
                d.expr(value);
            }),
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                simple,
            } => self.node("AnnAssign", &mut |d| {
                d.field("target", true);
                d.expr(target);
                d.field("annotation", false);
                d.expr(annotation);
                d.field("value", false);
                d.opt_expr(value);
                d.field("simple", false);
                write!(d.out, "{}", simple as u8).unwrap();
            }),
            StmtKind::For {
                is_async,
                target,
                iter,
                body,
                orelse,
            } => self.node(if is_async { "AsyncFor" } else { "For" }, &mut |d| {
                d.field("target", true);
                d.expr(target);
                d.field("iter", false);
                d.expr(iter);
                d.body_fields("body", body, false);
                d.body_fields("orelse", orelse, false);
            }),
            StmtKind::While { test, body, orelse } => self.node("While", &mut |d| {
                d.field("test", true);
                d.expr(test);
                d.body_fields("body", body, false);
                d.body_fields("orelse", orelse, false);
            }),
            StmtKind::If { test, body, orelse } => self.node("If", &mut |d| {
                d.field("test", true);
                d.expr(test);
                d.body_fields("body", body, false);
                d.body_fields("orelse", orelse, false);
            }),
            StmtKind::With {
                is_async,
                items,
                body,
            } => self.node(if is_async { "AsyncWith" } else { "With" }, &mut |d| {
                d.field("items", true);
                d.out.push('[');
                for (i, item) in m.list(items).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    let item = *item;
                    d.node("withitem", &mut |d| {
                        d.field("context_expr", true);
                        d.expr(item.context_expr);
                        d.field("optional_vars", false);
                        d.opt_expr(item.optional_vars);
                    });
                }
                d.out.push(']');
                d.body_fields("body", body, false);
            }),
            StmtKind::Match { subject, cases } => self.node("Match", &mut |d| {
                d.field("subject", true);
                d.expr(subject);
                d.field("cases", false);
                d.out.push('[');
                for (i, case) in m.list(cases).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    let case = *case;
                    d.node("match_case", &mut |d| {
                        d.field("pattern", true);
                        d.pattern(case.pattern);
                        d.field("guard", false);
                        d.opt_expr(case.guard);
                        d.body_fields("body", case.body, false);
                    });
                }
                d.out.push(']');
            }),
            StmtKind::Raise { exc, cause } => self.node("Raise", &mut |d| {
                d.field("exc", true);
                d.opt_expr(exc);
                d.field("cause", false);
                d.opt_expr(cause);
            }),
            StmtKind::Try {
                star,
                body,
                handlers,
                orelse,
                finalbody,
            } => self.node(if star { "TryStar" } else { "Try" }, &mut |d| {
                d.body_fields("body", body, true);
                d.field("handlers", false);
                d.out.push('[');
                for (i, h) in m.list(handlers).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    let h = *h;
                    d.node("ExceptHandler", &mut |d| {
                        d.field("type", true);
                        d.opt_expr(h.type_);
                        d.field("name", false);
                        d.opt_sym(h.name);
                        d.body_fields("body", h.body, false);
                    });
                }
                d.out.push(']');
                d.body_fields("orelse", orelse, false);
                d.body_fields("finalbody", finalbody, false);
            }),
            StmtKind::Assert { test, msg } => self.node("Assert", &mut |d| {
                d.field("test", true);
                d.expr(test);
                d.field("msg", false);
                d.opt_expr(msg);
            }),
            StmtKind::Import { names } => self.node("Import", &mut |d| {
                d.field("names", true);
                d.aliases(names);
            }),
            StmtKind::ImportFrom {
                module,
                names,
                level,
            } => self.node("ImportFrom", &mut |d| {
                d.field("module", true);
                d.opt_sym(module);
                d.field("names", false);
                d.aliases(names);
                d.field("level", false);
                write!(d.out, "{level}").unwrap();
            }),
            StmtKind::Global { names } | StmtKind::Nonlocal { names } => {
                let kind = if matches!(s.kind, StmtKind::Global { .. }) {
                    "Global"
                } else {
                    "Nonlocal"
                };
                self.node(kind, &mut |d| {
                    d.field("names", true);
                    d.out.push('[');
                    for (i, &n) in m.list(names).iter().enumerate() {
                        if i > 0 {
                            d.out.push_str(", ");
                        }
                        d.sym(n);
                    }
                    d.out.push(']');
                })
            }
            StmtKind::Expr { value } => self.node("Expr", &mut |d| {
                d.field("value", true);
                d.expr(value);
            }),
            StmtKind::Pass => self.node("Pass", &mut |_| {}),
            StmtKind::Break => self.node("Break", &mut |_| {}),
            StmtKind::Continue => self.node("Continue", &mut |_| {}),
        }
    }

    fn aliases(&mut self, list: List<Alias>) {
        self.out.push('[');
        for (i, a) in self.m.list(list).iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            let a = *a;
            self.node("alias", &mut |d| {
                d.field("name", true);
                d.sym(a.name);
                d.field("asname", false);
                d.opt_sym(a.asname);
            });
        }
        self.out.push(']');
    }

    fn pattern(&mut self, id: PatId) {
        let m = self.m;
        let p = *m.pattern(id);
        let pats = |d: &mut Self, list: List<PatId>| {
            d.out.push('[');
            for (i, &p) in m.list(list).iter().enumerate() {
                if i > 0 {
                    d.out.push_str(", ");
                }
                d.pattern(p);
            }
            d.out.push(']');
        };
        match p.kind {
            PatternKind::MatchValue { value } => self.node("MatchValue", &mut |d| {
                d.field("value", true);
                d.expr(value);
            }),
            PatternKind::MatchSingleton { value } => self.node("MatchSingleton", &mut |d| {
                d.field("value", true);
                d.out.push_str(match value {
                    Constant::True => "true",
                    Constant::False => "false",
                    _ => "null",
                });
            }),
            PatternKind::MatchSequence { patterns } => self.node("MatchSequence", &mut |d| {
                d.field("patterns", true);
                pats(d, patterns);
            }),
            PatternKind::MatchMapping {
                keys,
                patterns,
                rest,
            } => self.node("MatchMapping", &mut |d| {
                d.field("keys", true);
                d.exprs(keys);
                d.field("patterns", false);
                pats(d, patterns);
                d.field("rest", false);
                d.opt_sym(rest);
            }),
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
            } => self.node("MatchClass", &mut |d| {
                d.field("cls", true);
                d.expr(cls);
                d.field("patterns", false);
                pats(d, patterns);
                d.field("kwd_attrs", false);
                d.out.push('[');
                for (i, &n) in m.list(kwd_attrs).iter().enumerate() {
                    if i > 0 {
                        d.out.push_str(", ");
                    }
                    d.sym(n);
                }
                d.out.push(']');
                d.field("kwd_patterns", false);
                pats(d, kwd_patterns);
            }),
            PatternKind::MatchStar { name } => self.node("MatchStar", &mut |d| {
                d.field("name", true);
                d.opt_sym(name);
            }),
            PatternKind::MatchAs { pattern, name } => self.node("MatchAs", &mut |d| {
                d.field("pattern", true);
                match pattern {
                    Some(p) => d.pattern(p),
                    None => d.out.push_str("null"),
                }
                d.field("name", false);
                d.opt_sym(name);
            }),
            PatternKind::MatchOr { patterns } => self.node("MatchOr", &mut |d| {
                d.field("patterns", true);
                pats(d, patterns);
            }),
        }
    }
}
