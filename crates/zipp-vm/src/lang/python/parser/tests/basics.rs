//! Unit checks of the front end: tokens, trees, errors, `\N{...}` names and
//! deep nesting. `conformance.rs` checks whole programs against CPython and
//! the recorded goldens.

use zipp_pyparse::ast::*;
use zipp_pyparse::token::T;
use zipp_pyparse::{lexer, parse, Limits, Mode};

fn module(source: &str) -> Module<'_> {
    parse(source, Mode::Module, 0, Limits::NONE).unwrap_or_else(|e| panic!("{source:?}: {e:?}"))
}

fn error(source: &str) -> (String, u32) {
    match parse(source, Mode::Module, 0, Limits::NONE) {
        Ok(_) => panic!("{source:?} parsed"),
        Err(e) => (e.message, e.offset),
    }
}

fn kinds(source: &str) -> Vec<T> {
    lexer::lex(source, Mode::Module, 0, Limits::NONE)
        .tokens
        .iter()
        .map(|t| t.kind)
        .collect()
}

/// The single expression statement's expression.
fn expr<'m>(m: &'m Module) -> &'m Expr {
    let body = m.list(m.body);
    assert_eq!(body.len(), 1);
    match m.stmt(body[0]).kind {
        StmtKind::Expr { value } => m.expr(value),
        ref other => panic!("not an expression statement: {other:?}"),
    }
}

#[test]
fn tokens() {
    use T::*;
    assert_eq!(
        kinds("if x:\n    y = 1\n"),
        [If, Name, Colon, Newline, Indent, Name, Equal, Int, Newline, Dedent, EndOfFile]
    );
    // Newlines inside brackets are not logical.
    assert_eq!(
        kinds("(a,\n b)\n"),
        [Lpar, Name, Comma, Name, Rpar, Newline, EndOfFile]
    );
    // Soft keywords are names unless they start a statement of their own.
    assert_eq!(kinds("match = 1\n")[0], Name);
    assert_eq!(kinds("match x:\n case 1: pass\n")[0], Match);
    assert_eq!(kinds("type X = int\n")[0], Type);
    assert_eq!(kinds("type(x)\n")[0], Name);
    // PEP 701: an f-string is tokens, nested quotes and all.
    assert_eq!(
        kinds("f'{x!r:>{w}} {\"y\"}'\n"),
        [
            FStringStart,
            FieldStart,
            Name,
            Exclamation,
            Name,
            FormatSpec,
            FStringMiddle,
            FieldStart,
            Name,
            FieldEnd,
            FieldEnd,
            FStringMiddle,
            FieldStart,
            String,
            FieldEnd,
            FStringEnd,
            Newline,
            EndOfFile
        ]
    );
}

#[test]
fn ranges_and_lines() {
    let source = "x = 1\nprint(x + 2)\n";
    let m = module(source);
    let body = m.list(m.body);
    assert_eq!(body.len(), 2);
    let call = m.stmt(body[1]);
    assert_eq!(m.text(call.range), "print(x + 2)");
    assert_eq!(m.line_col(call.range.start), (2, 0));
}

#[test]
fn numbers_are_read_from_their_text() {
    let m = module("0x_ff\n");
    match expr(&m).kind {
        ExprKind::Constant(Constant::Int { radix, text }) => {
            assert_eq!(radix, 16);
            assert_eq!(m.int_digits(radix, text).0, "ff");
        }
        ref other => panic!("{other:?}"),
    }
    let m = module("1_000.5e-3\n");
    match expr(&m).kind {
        ExprKind::Constant(Constant::Float { text }) => assert_eq!(m.float_value(text), 1.0005),
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn fstring_debug_text_leaves_out_comments() {
    // As CPython 3.12+: a comment runs to the end of the line, even in a
    // single-quoted f-string, and `=` keeps the rest of the text.
    let m = module("f\"{1+2 = # my comment\n  }\"\n");
    let ExprKind::JoinedStr { values } = expr(&m).kind else {
        panic!("not an f-string")
    };
    let values = m.list(values);
    match m.expr(values[0]).kind {
        ExprKind::Constant(Constant::Str { value, .. }) => assert_eq!(m.str(value), "1+2 = \n  "),
        ref other => panic!("{other:?}"),
    }
    module("f'{x # c\"\n}'\n");
    module("f'{x # c}\n}'\n");
    // The comment swallows the closing brace and quote: never closed.
    error("f'{x # c}'\n");
}

#[test]
fn errors_have_messages_and_offsets() {
    assert_eq!(error("x = = 1\n").1, 4);
    let (message, offset) = error("def f(a=1, b): pass\n");
    assert_eq!(message, "non-default argument follows default argument");
    assert_eq!(offset, 11);
    let (message, _) = error("if x:\npass\n");
    assert!(message.contains("expected an indented block"), "{message}");
}

#[test]
fn limits_end_the_program() {
    let limits = Limits {
        max_tokens: usize::MAX,
        max_brackets: 3,
        max_indent: 100,
    };
    let e = parse("x = ((((1))))\n", Mode::Module, 0, limits)
        .err()
        .unwrap();
    assert!(e.limit);
    assert!(parse("x = (((1)))\n", Mode::Module, 0, limits).is_ok());
}

#[test]
fn python_3_13_syntax() {
    module("type Pair[T: (int, str), *Ts, **P] = tuple[T, *Ts]\n");
    module("def f[T = int](x: T) -> T: return x\n");
    module("try:\n    pass\nexcept* ValueError as e:\n    pass\n");
    module("match p:\n    case Point(x=0, y=0) | [1, *_] | {'k': v, **rest}: pass\n");
    module("f'{'a' 'b'}{f'{1:{2}}'}'\n");
    module("f\"{'\\n'.join(x)}\"\n");
    module("f'''{\n    x  # a comment\n}'''\n");
    module("async def f():\n    async with a as b, c:\n        await d\n");
}

/// The `\N{...}` table: every name looks up its character (in any case),
/// and the table is the one the RustPython fork's parser shipped (2,745
/// names and aliases; the fingerprint of that set, sorted, was taken from
/// the fork's table before it was retired). `conformance` checks each name
/// against CPython's `unicodedata`.
#[test]
fn unicode_names() {
    let mut all = zipp_pyparse::unicode_names::all();
    for (name, c) in &all {
        assert_eq!(
            zipp_pyparse::unicode_names::lookup(name),
            Some(*c),
            "{name}"
        );
        assert_eq!(
            zipp_pyparse::unicode_names::lookup(&name.to_lowercase()),
            Some(*c)
        );
    }
    assert_eq!(all.len(), 2745);
    all.sort_by(|a, b| (a.1, a.0.to_uppercase()).cmp(&(b.1, b.0.to_uppercase())));
    let text: String = all
        .iter()
        .map(|(name, c)| format!("{:04X} {}\n", *c as u32, name.to_uppercase()))
        .collect();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(h, 0x65ef_6acd_9030_d50b, "the name set changed");
    for (name, c) in [
        ("EN SPACE", '\u{2002}'),
        ("bel", '\u{7}'),
        ("LATIN SMALL LETTER E WITH ACUTE", '\u{e9}'),
        ("GREEK SMALL LETTER ALPHA", '\u{3b1}'),
        ("SNOWMAN", '\u{2603}'),
        ("CJK UNIFIED IDEOGRAPH-4E00", '\u{4E00}'),
    ] {
        assert_eq!(zipp_pyparse::unicode_names::lookup(name), Some(c), "{name}");
    }
    assert_eq!(
        zipp_pyparse::unicode_names::lookup("NO SUCH CHARACTER"),
        None
    );
}

/// Deep right-nested chains do not recurse (ZIPP runs the parser on small
/// stacks); deep bracket nesting is bounded instead.
#[test]
fn deep_chains_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            for (prefix, repeat, suffix) in [
                ("x = ", "-", "1"),
                ("x = ", "not ", "1"),
                ("x = a", ".b", ""),
                ("x = f", "(1)", ""),
                ("x = 1", " + 1", ""),
                ("x = ", "a if b else ", "c"),
                ("x = ", "lambda: ", "1"),
            ] {
                let source = format!("{prefix}{}{suffix}\n", repeat.repeat(100_000));
                module(&source);
            }
        })
        .unwrap()
        .join()
        .unwrap();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let deep = format!("x = {}1{}\n", "(".repeat(5000), ")".repeat(5000));
            error(&deep);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// F-strings nest at most 149 deep, as in CPython: the lexer and the parser
/// recurse per level, and the limits do not count inside an f-string. (149
/// levels need 256-512 KB of stack in a release build.)
#[test]
fn fstring_nesting_is_bounded() {
    fn nested(n: usize) -> String {
        let q = ["'", "\""];
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!("f{}{{", q[i % 2]));
        }
        s.push('1');
        for i in (0..n).rev() {
            s.push_str(&format!("}}{}", q[i % 2]));
        }
        s + "
"
    }
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| {
            module(&nested(149));
            for n in [150, 100_000] {
                // At the 150th literal's quote, as CPython reports it.
                assert_eq!(
                    error(&nested(n)),
                    ("too many nested f-strings".to_owned(), 448)
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The emitter's tree: RustPython-shaped nodes with the parser's ranges,
/// PEP 696 defaults included.
#[test]
fn tree_nodes() {
    use zipp_pyparse::tree::{self, Bump, Constant, Expr, Ranged, Stmt, TypeParam};
    let source = "def f[T: int = str, *Ts = (), **P = []](a, b=0x_ff): return -a\n";
    let module = module(source);
    let bump = Bump::new();
    let body = tree::build(&module, &bump).unwrap();
    let Stmt::FunctionDef(f) = &body[0] else {
        panic!("not a function")
    };
    assert_eq!(f.name.as_str(), "f");
    assert_eq!(f.range().start(), 0);
    let [TypeParam::TypeVar(t), TypeParam::TypeVarTuple(ts), TypeParam::ParamSpec(p)] =
        f.type_params.as_slice()
    else {
        panic!("{:?}", f.type_params)
    };
    assert!(t.bound.is_some() && t.default.is_some());
    assert!(ts.default.is_some() && p.default.is_some());
    let Some(Expr::Constant(c)) = f.args.args[1].default else {
        panic!("no default")
    };
    let Constant::Int(i) = c.value else {
        panic!("not an int")
    };
    assert_eq!((i.to_string(), i.text()), ("255".to_owned(), "0x_ff"));
    assert_eq!(
        tree::IntLit::to_string(&i),
        "255",
        "decimal display of a hex literal"
    );
    let Stmt::Return(r) = &f.body[0] else {
        panic!("no return")
    };
    let value = r.value.unwrap();
    assert_eq!(&source[value.start() as usize..value.end() as usize], "-a");
}
