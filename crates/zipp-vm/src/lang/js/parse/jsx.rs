//! JSX: elements in expressions, in `.jsx` and `.tsx` files. An element is
//! read where a primary expression starts and lowered as it is read, to the
//! calls tsc emits for it, so the tree is the one the compiled JavaScript
//! would give and nothing after the parser knows JSX exists.
//!
//! JSX text, attribute strings and names are not JavaScript tokens (`don't`
//! in text is no unterminated string, `// x` no comment, `data-x` one name,
//! and an attribute string has no escapes), so they are read here from the
//! source bytes, and the lexer is handed back its position only for the
//! JavaScript inside `{…}` (`Parser::resume_at`).
//!
//! The two lowerings are tsc's, chosen by tsconfig's `jsx`:
//!
//! - **classic** (`react`): `React.createElement(tag, props, ...children)`,
//!   through `jsxFactory` and `jsxFragmentFactory`, which a file's `@jsx`
//!   and `@jsxFrag` pragmas override;
//! - **automatic** (`react-jsx`, `react-jsxdev`): `_jsx(tag, { ...props,
//!   children }, key)`, `_jsxs` for several children, imported from
//!   `<jsxImportSource>/jsx-runtime` (`react` unless the config or an
//!   `@jsxImportSource` pragma names another). A `key` after a spread
//!   falls back to the source's own `createElement`, as React needs.
//!
//! Text is trimmed as tsc trims it: whitespace touching a line break is
//! dropped, lines left empty are dropped and the rest are joined by one
//! space; HTML character references (`&amp;`, `&#169;`) are decoded in
//! text and in attribute strings.

use super::ast::*;
use super::lexer::Lexer;
use super::parser::{PResult, Parser, SyntaxError};
use super::token::{Punct, StrVal};

/// How a file's JSX becomes calls: tsconfig's `jsx`, `jsxFactory`,
/// `jsxFragmentFactory` and `jsxImportSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsxOptions {
    /// The automatic runtime (`react-jsx`) rather than the factory.
    pub automatic: bool,
    /// `react-jsxdev`: `jsxDEV` from `jsx-dev-runtime`.
    pub dev: bool,
    /// The classic runtime's factory and fragment, as dotted names.
    pub factory: &'static str,
    pub fragment: &'static str,
    /// The automatic runtime's package.
    pub import_source: &'static str,
}

impl Default for JsxOptions {
    /// React's automatic runtime: what a component that does not import
    /// React (as most are written now) needs, and what a project that leaves
    /// lowering to its bundler (`jsx: "preserve"`) gets from it.
    fn default() -> JsxOptions {
        JsxOptions {
            automatic: true,
            dev: false,
            factory: "React.createElement",
            fragment: "React.Fragment",
            import_source: "react",
        }
    }
}

/// The automatic runtime's names, which a file imports when it uses them.
#[derive(Clone, Copy)]
enum Rt {
    Jsx,
    Jsxs,
    JsxDev,
    Fragment,
    CreateElement,
}

impl Rt {
    const ALL: [Rt; 5] = [
        Rt::Jsx,
        Rt::Jsxs,
        Rt::JsxDev,
        Rt::Fragment,
        Rt::CreateElement,
    ];

    fn export(self) -> &'static str {
        match self {
            Rt::Jsx => "jsx",
            Rt::Jsxs => "jsxs",
            Rt::JsxDev => "jsxDEV",
            Rt::Fragment => "Fragment",
            Rt::CreateElement => "createElement",
        }
    }

    /// The local binding: not an identifier a program can write, so it
    /// shadows nothing and nothing shadows it.
    fn local(self) -> &'static str {
        match self {
            Rt::Jsx => "%jsx",
            Rt::Jsxs => "%jsxs",
            Rt::JsxDev => "%jsxDEV",
            Rt::Fragment => "%Fragment",
            Rt::CreateElement => "%createElement",
        }
    }
}

/// A file's JSX settings, its pragmas applied, and the runtime names its
/// elements have used.
pub(crate) struct JsxState {
    /// The automatic runtime's package; `None` for the classic runtime.
    source: Option<String>,
    dev: bool,
    factory: Vec<String>,
    fragment: Vec<String>,
    used: [bool; 5],
}

impl JsxState {
    pub(crate) fn new(o: JsxOptions, src: &str) -> JsxState {
        let mut st = JsxState {
            source: o.automatic.then(|| o.import_source.to_string()),
            dev: o.dev,
            factory: dotted(o.factory),
            fragment: dotted(o.fragment),
            used: [false; 5],
        };
        let p = pragmas(src);
        if let Some(f) = p.factory {
            st.factory = dotted(f);
        }
        if let Some(f) = p.fragment {
            st.fragment = dotted(f);
        }
        // tsc's `getJSXImplicitImportBase`: `@jsxRuntime classic` turns the
        // automatic runtime off; `@jsxRuntime automatic` or an
        // `@jsxImportSource` turns it on. An `@jsx` factory alone asks for
        // the classic runtime, as Babel and esbuild read it.
        match p.runtime {
            Some(r) if r.eq_ignore_ascii_case("classic") => st.source = None,
            Some(r) if r.eq_ignore_ascii_case("automatic") => {
                let source = p.import_source.unwrap_or(o.import_source);
                st.source = Some(source.to_string());
            }
            _ => {
                if let Some(s) = p.import_source {
                    st.source = Some(s.to_string());
                } else if p.factory.is_some() {
                    st.source = None;
                }
            }
        }
        st
    }
}

fn dotted(s: &str) -> Vec<String> {
    s.split('.').map(str::to_string).collect()
}

/// The JSX pragmas among a file's leading comments, as tsc reads them (the
/// last of each kind wins).
#[derive(Default)]
struct Pragmas<'a> {
    factory: Option<&'a str>,
    fragment: Option<&'a str>,
    import_source: Option<&'a str>,
    runtime: Option<&'a str>,
}

fn pragmas(src: &str) -> Pragmas<'_> {
    let mut out = Pragmas::default();
    let mut rest = src.strip_prefix('\u{feff}').unwrap_or(src);
    if rest.starts_with("#!") {
        rest = rest.find('\n').map_or("", |i| &rest[i..]);
    }
    loop {
        rest = rest.trim_start();
        let comment = if let Some(r) = rest.strip_prefix("//") {
            let end = r.find(['\n', '\r']).unwrap_or(r.len());
            rest = &r[end..];
            &r[..end]
        } else if let Some(r) = rest.strip_prefix("/*") {
            let Some(end) = r.find("*/") else { break };
            rest = &r[end + 2..];
            &r[..end]
        } else {
            break;
        };
        for (at, _) in comment.match_indices('@') {
            let tail = &comment[at + 1..];
            let word_len = tail
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(tail.len());
            let word = &tail[..word_len];
            let after = &tail[word_len..];
            if !after.starts_with(|c: char| c.is_whitespace()) {
                continue;
            }
            let arg = after.trim_start();
            let arg = &arg[..arg.find(char::is_whitespace).unwrap_or(arg.len())];
            let arg = arg.trim_end_matches('*');
            if arg.is_empty() {
                continue;
            }
            let slot = match word.to_ascii_lowercase().as_str() {
                "jsx" => &mut out.factory,
                "jsxfrag" => &mut out.fragment,
                "jsximportsource" => &mut out.import_source,
                "jsxruntime" => &mut out.runtime,
                _ => continue,
            };
            *slot = Some(arg);
        }
    }
    out
}

/// A JSX element's name, as written: `div`, `Foo.Bar`, `svg:rect`.
struct Tag {
    text: String,
    namespaced: bool,
}

/// An attribute: `name`, `name="…"`, `name={…}`, or `{...spread}`.
enum Attr {
    Prop(String, Expr),
    Spread(Expr),
}

/// A child: text, `{expr}`, an element, or `{...spread}`.
enum Child {
    Expr(Expr),
    Spread(Expr),
}

impl<'s> Parser<'s> {
    /// At `<` where a primary expression starts: the element or fragment
    /// that begins there, lowered, with the token after it current.
    pub(crate) fn parse_jsx(&mut self) -> PResult<Expr> {
        let lt = self.cur().span.start;
        let (e, end) = self.jsx_element_at(lt, self.prev_end())?;
        // After an element comes an operator: a `/` divides.
        self.resume_at(end, false)?;
        Ok(e)
    }

    fn jsx_byte(&self, pos: u32) -> u8 {
        self.source()
            .as_bytes()
            .get(pos as usize)
            .copied()
            .unwrap_or(0)
    }

    /// The element whose `<` is at `lt`, lowered, and the position after it.
    /// `loc` is where tsc's node for it starts, the trivia before the `<`
    /// included: `react-jsxdev` reports that line and column.
    fn jsx_element_at(&mut self, lt: u32, loc: u32) -> PResult<(Expr, u32)> {
        self.with_syntax_recursion(|p| p.jsx_element(lt, loc))
    }

    fn jsx_element(&mut self, lt: u32, loc: u32) -> PResult<(Expr, u32)> {
        let mut pos = self.skip_trivia_from(lt + 1)?;
        if self.jsx_byte(pos) == b'>' {
            let (children, end) = self.jsx_children(pos + 1, None)?;
            return Ok((self.jsx_lower(None, Vec::new(), children, lt, loc), end));
        }
        let (tag, after) = self.jsx_name(pos)?;
        pos = self.skip_trivia_from(after)?;
        // `<Select<Option> …>`: type arguments, read and dropped.
        if self.ts && self.jsx_byte(pos) == b'<' {
            self.resume_at(pos, false)?;
            self.ts_type_args()?;
            pos = self.cur().span.start;
        }
        let tag_expr = self.jsx_tag_expr(&tag);
        let mut attrs = Vec::new();
        loop {
            pos = self.skip_trivia_from(pos)?;
            match self.jsx_byte(pos) {
                b'/' => {
                    let gt = self.skip_trivia_from(pos + 1)?;
                    self.jsx_expect_gt(gt)?;
                    let e = self.jsx_lower(Some(tag_expr), attrs, Vec::new(), lt, loc);
                    return Ok((e, gt + 1));
                }
                b'>' => {
                    let (children, end) = self.jsx_children(pos + 1, Some(&tag))?;
                    return Ok((
                        self.jsx_lower(Some(tag_expr), attrs, children, lt, loc),
                        end,
                    ));
                }
                b'{' => {
                    self.resume_at(pos + 1, true)?;
                    if !self.at(Punct::DotDotDot) {
                        return Err(
                            self.err_here("SyntaxError: expected '...' in a JSX spread attribute")
                        );
                    }
                    self.bump_before_operand()?;
                    let e = self.parse_expr_full()?;
                    pos = self.jsx_close_brace()?;
                    attrs.push(Attr::Spread(e));
                }
                _ if pos >= self.src_len() => {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: unterminated JSX tag <{}>", tag.text),
                        lt,
                    ));
                }
                _ => {
                    let (name, after) = self.jsx_attr_name(pos)?;
                    let eq = self.skip_trivia_from(after)?;
                    if self.jsx_byte(eq) != b'=' {
                        attrs.push(Attr::Prop(name, Expr::Bool(true)));
                        pos = after;
                        continue;
                    }
                    let at = self.skip_trivia_from(eq + 1)?;
                    let (value, end) = match self.jsx_byte(at) {
                        q @ (b'"' | b'\'') => {
                            let body = at as usize + 1;
                            let Some(len) = self.source()[body..].find(q as char) else {
                                return Err(SyntaxError::new(
                                    "SyntaxError: unterminated string in a JSX attribute",
                                    at,
                                ));
                            };
                            let raw = &self.source()[body..body + len];
                            (Expr::Str(decode_entities(raw)), (body + len + 1) as u32)
                        }
                        b'{' => {
                            self.resume_at(at + 1, true)?;
                            if self.at(Punct::RBrace) {
                                return Err(self.err_here(
                                    "SyntaxError: JSX attributes must only be assigned a non-empty expression",
                                ));
                            }
                            let e = self.parse_expr_full()?;
                            (e, self.jsx_close_brace()?)
                        }
                        b'<' => self.jsx_element_at(at, eq + 1)?,
                        _ => {
                            return Err(SyntaxError::new(
                                "SyntaxError: expected a JSX attribute value",
                                at,
                            ))
                        }
                    };
                    attrs.push(Attr::Prop(name, value));
                    pos = end;
                }
            }
        }
    }

    /// The children up to the closing tag of `open` (`None`: a fragment's
    /// `</>`), and the position after that tag.
    fn jsx_children(&mut self, mut pos: u32, open: Option<&Tag>) -> PResult<(Vec<Child>, u32)> {
        let mut out = Vec::new();
        let len = self.src_len();
        loop {
            let start = pos;
            let bytes = self.source().as_bytes();
            while pos < len {
                match bytes[pos as usize] {
                    b'{' | b'<' => break,
                    c @ (b'>' | b'}') => {
                        let c = c as char;
                        return Err(SyntaxError::new(
                            format!("SyntaxError: unexpected '{c}' in JSX text (write {{'{c}'}} for the character)"),
                            pos,
                        ));
                    }
                    _ => pos += 1,
                }
            }
            if pos >= len {
                let what = open.map_or_else(|| "<>".to_string(), |t| format!("<{}>", t.text));
                return Err(SyntaxError::new(
                    format!("SyntaxError: {what} has no closing tag"),
                    start,
                ));
            }
            if pos > start {
                if let Some(text) = jsx_text(&self.source()[start as usize..pos as usize]) {
                    out.push(Child::Expr(Expr::Str(text)));
                }
            }
            if bytes[pos as usize] == b'{' {
                self.resume_at(pos + 1, true)?;
                // `{}` and `{/* comment */}` are nothing.
                if self.at(Punct::RBrace) {
                    pos = self.cur().span.end;
                    continue;
                }
                let spread = self.at(Punct::DotDotDot);
                if spread {
                    self.bump_before_operand()?;
                }
                let e = self.parse_expr_full()?;
                pos = self.jsx_close_brace()?;
                out.push(if spread {
                    Child::Spread(e)
                } else {
                    Child::Expr(e)
                });
                continue;
            }
            let after = self.skip_trivia_from(pos + 1)?;
            if self.jsx_byte(after) == b'/' {
                let mut gt = self.skip_trivia_from(after + 1)?;
                if let Some(tag) = open {
                    let (close, end) = self.jsx_name(gt)?;
                    if close.text != tag.text {
                        return Err(SyntaxError::new(
                            format!(
                                "SyntaxError: expected corresponding JSX closing tag for '{}'",
                                tag.text
                            ),
                            gt,
                        ));
                    }
                    gt = self.skip_trivia_from(end)?;
                }
                self.jsx_expect_gt(gt)?;
                return Ok((out, gt + 1));
            }
            let (e, end) = self.jsx_element_at(pos, pos)?;
            out.push(Child::Expr(e));
            pos = end;
        }
    }

    fn jsx_expect_gt(&self, pos: u32) -> PResult<()> {
        if self.jsx_byte(pos) == b'>' {
            Ok(())
        } else {
            Err(SyntaxError::new("SyntaxError: expected '>'", pos))
        }
    }

    /// The `}` ending a `{…}` is current: the position after it, where the
    /// JSX resumes.
    fn jsx_close_brace(&self) -> PResult<u32> {
        if !self.at(Punct::RBrace) {
            return Err(self.err_here("SyntaxError: expected '}'"));
        }
        Ok(self.cur().span.end)
    }

    /// The end of the JSX identifier at `pos` (an IdentifierName that may
    /// also hold `-`), if one starts there.
    fn jsx_ident(&self, pos: u32) -> Option<u32> {
        let s = self.source().get(pos as usize..)?;
        let mut chars = s.char_indices();
        let (_, c) = chars.next()?;
        if !Lexer::is_id_start(c) {
            return None;
        }
        let mut end = c.len_utf8();
        for (i, c) in chars {
            if c != '-' && !Lexer::is_id_part(c) {
                break;
            }
            end = i + c.len_utf8();
        }
        Some(pos + end as u32)
    }

    /// The element name at `pos` (`a`, `a.b.c`, `a:b`) and the position after it.
    fn jsx_name(&mut self, pos: u32) -> PResult<(Tag, u32)> {
        let Some(mut end) = self.jsx_ident(pos) else {
            return Err(SyntaxError::new(
                "SyntaxError: expected a JSX element name",
                pos,
            ));
        };
        let mut text = self.src_text(pos, end).to_string();
        let mut p = self.skip_trivia_from(end)?;
        if self.jsx_byte(p) == b':' {
            let q = self.skip_trivia_from(p + 1)?;
            let Some(e) = self.jsx_ident(q) else {
                return Err(SyntaxError::new(
                    "SyntaxError: expected a name after ':'",
                    q,
                ));
            };
            text.push(':');
            text.push_str(self.src_text(q, e));
            return Ok((
                Tag {
                    text,
                    namespaced: true,
                },
                e,
            ));
        }
        while self.jsx_byte(p) == b'.' {
            let q = self.skip_trivia_from(p + 1)?;
            let Some(e) = self.jsx_ident(q) else {
                return Err(SyntaxError::new(
                    "SyntaxError: expected a name after '.'",
                    q,
                ));
            };
            text.push('.');
            text.push_str(self.src_text(q, e));
            end = e;
            p = self.skip_trivia_from(e)?;
        }
        Ok((
            Tag {
                text,
                namespaced: false,
            },
            end,
        ))
    }

    /// The attribute name at `pos` (`a`, `data-a`, `xlink:href`) and the
    /// position after it.
    fn jsx_attr_name(&mut self, pos: u32) -> PResult<(String, u32)> {
        let Some(end) = self.jsx_ident(pos) else {
            return Err(SyntaxError::new(
                "SyntaxError: expected a JSX attribute",
                pos,
            ));
        };
        let mut name = self.src_text(pos, end).to_string();
        let p = self.skip_trivia_from(end)?;
        if self.jsx_byte(p) != b':' {
            return Ok((name, end));
        }
        let q = self.skip_trivia_from(p + 1)?;
        let Some(e) = self.jsx_ident(q) else {
            return Err(SyntaxError::new(
                "SyntaxError: expected a name after ':'",
                q,
            ));
        };
        name.push(':');
        name.push_str(self.src_text(q, e));
        Ok((name, e))
    }

    /// What a tag evaluates to: a string for an intrinsic element (`div`,
    /// `my-el`, `svg:rect`), else the component it names.
    fn jsx_tag_expr(&mut self, tag: &Tag) -> Expr {
        if tag.namespaced {
            return str_expr(&tag.text);
        }
        let mut parts = tag.text.split('.');
        let first = parts.next().unwrap_or_default();
        let mut e = if first == "this" {
            Expr::This
        } else if !tag.text.contains('.')
            && (first.as_bytes()[0].is_ascii_lowercase() || first.contains('-'))
        {
            return str_expr(first);
        } else {
            self.jsx_ref(first)
        };
        for part in parts {
            let prop = if part.contains('-') {
                MemberProp::Computed(Box::new(str_expr(part)))
            } else {
                MemberProp::Ident(self.intern_name(part))
            };
            e = Expr::Member(Box::new(Member {
                object: e,
                prop,
                optional: false,
            }));
        }
        e
    }

    /// A reference to `name` in the program's scope (a component, the
    /// factory's namespace), counted as a use of an import of it.
    fn jsx_ref(&mut self, name: &str) -> Expr {
        let n = self.intern_name(name);
        if self.ts {
            self.ts_note_ref(&n);
            if !self.ts_state.legacy_classes.is_empty() {
                if let Some(alias) = self.ts_class_alias(&n) {
                    return Expr::Ident(alias);
                }
            }
        }
        Expr::Ident(n)
    }

    /// `a.b.c` from its parts.
    fn jsx_path(&mut self, path: &[String]) -> Expr {
        let mut e = self.jsx_ref(&path[0]);
        for part in &path[1..] {
            e = Expr::Member(Box::new(Member {
                object: e,
                prop: MemberProp::Ident(self.intern_name(part)),
                optional: false,
            }));
        }
        e
    }

    /// A name from the automatic runtime, imported once the program is read.
    fn jsx_runtime(&mut self, rt: Rt) -> Expr {
        if let Some(st) = self.jsx.as_mut() {
            st.used[rt as usize] = true;
        }
        Expr::Ident(self.intern_name(rt.local()))
    }

    /// The call an element (`tag` given) or fragment becomes.
    fn jsx_lower(
        &mut self,
        tag: Option<Expr>,
        attrs: Vec<Attr>,
        children: Vec<Child>,
        pos: u32,
        loc: u32,
    ) -> Expr {
        let (automatic, dev) = match self.jsx.as_deref() {
            Some(st) => (st.source.is_some(), st.dev),
            None => (false, false),
        };
        if !automatic {
            return self.jsx_classic(tag, attrs, children, pos, false);
        }
        if tag.is_some() && key_after_spread(&attrs) {
            return self.jsx_classic(tag, attrs, children, pos, true);
        }
        let is_static = children.len() > 1 || matches!(children.first(), Some(Child::Spread(_)));
        let children_prop = if children.is_empty() {
            None
        } else if !is_static {
            let Some(Child::Expr(e)) = children.into_iter().next() else {
                unreachable!()
            };
            Some(e)
        } else {
            let items = children
                .into_iter()
                .map(|c| {
                    Some(match c {
                        Child::Expr(e) => ArrayElem::Expr(e),
                        Child::Spread(e) => ArrayElem::Spread(e),
                    })
                })
                .collect();
            Some(Expr::Array(items, LiteralFlags::default()))
        };
        let mut key = None;
        let mut members = Vec::new();
        for a in attrs {
            match a {
                Attr::Prop(name, v) if name == "key" && key.is_none() => key = Some(v),
                Attr::Prop(name, v) => members.push(self.jsx_prop(&name, v)),
                Attr::Spread(e) => push_spread(&mut members, e),
            }
        }
        if let Some(c) = children_prop {
            members.push(self.jsx_prop("children", c));
        }
        let callee = self.jsx_runtime(if dev {
            Rt::JsxDev
        } else if is_static {
            Rt::Jsxs
        } else {
            Rt::Jsx
        });
        let tag = match tag {
            Some(t) => t,
            None => self.jsx_runtime(Rt::Fragment),
        };
        let mut args = vec![
            Arg::Expr(tag),
            Arg::Expr(Expr::Object(members, LiteralFlags::default())),
        ];
        let has_key = key.is_some();
        if let Some(k) = key {
            args.push(Arg::Expr(k));
        }
        if dev {
            if !has_key {
                args.push(Arg::Expr(Expr::Unary {
                    op: UnaryOp::Void,
                    arg: Box::new(Expr::Num(0.0)),
                }));
            }
            args.push(Arg::Expr(Expr::Bool(is_static)));
            let before = &self.source()[..loc as usize];
            let line = before.matches('\n').count() + 1;
            let column = before
                .rsplit('\n')
                .next()
                .map_or(0, |l| l.encode_utf16().count())
                + 1;
            let source = vec![
                self.jsx_prop("fileName", str_expr("")),
                self.jsx_prop("lineNumber", Expr::Num(line as f64)),
                self.jsx_prop("columnNumber", Expr::Num(column as f64)),
            ];
            args.push(Arg::Expr(Expr::Object(source, LiteralFlags::default())));
            args.push(Arg::Expr(Expr::This));
        }
        call(callee, args, pos)
    }

    /// `factory(tag, props, ...children)`: the classic runtime, or the
    /// automatic runtime's `createElement` (`imported`).
    fn jsx_classic(
        &mut self,
        tag: Option<Expr>,
        attrs: Vec<Attr>,
        children: Vec<Child>,
        pos: u32,
        imported: bool,
    ) -> Expr {
        let callee = if imported {
            self.jsx_runtime(Rt::CreateElement)
        } else {
            let factory = self
                .jsx
                .as_ref()
                .map(|s| s.factory.clone())
                .unwrap_or_default();
            self.jsx_path(&factory)
        };
        let tag = match tag {
            Some(t) => t,
            None => {
                let fragment = self
                    .jsx
                    .as_ref()
                    .map(|s| s.fragment.clone())
                    .unwrap_or_default();
                self.jsx_path(&fragment)
            }
        };
        let props = if attrs.is_empty() {
            Expr::Null
        } else {
            let mut members = Vec::new();
            for a in attrs {
                match a {
                    Attr::Prop(name, v) => members.push(self.jsx_prop(&name, v)),
                    Attr::Spread(e) => push_spread(&mut members, e),
                }
            }
            Expr::Object(members, LiteralFlags::default())
        };
        let mut args = vec![Arg::Expr(tag), Arg::Expr(props)];
        args.extend(children.into_iter().map(|c| match c {
            Child::Expr(e) => Arg::Expr(e),
            Child::Spread(e) => Arg::Spread(e),
        }));
        call(callee, args, pos)
    }

    /// `name: value`, keyed by an identifier when the name is one (as tsc
    /// writes it), else by a string (`"aria-label"`, `"xlink:href"`).
    fn jsx_prop(&mut self, name: &str, value: Expr) -> ObjectMember {
        let b = name.as_bytes();
        let ident = (b[0].is_ascii_alphabetic() || b[0] == b'_')
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_');
        let key = if ident {
            PropKey::Ident(self.intern_name(name))
        } else {
            PropKey::Str(StrVal::Utf8(name.to_string()))
        };
        ObjectMember::Prop {
            key,
            value,
            shorthand: false,
            init: None,
        }
    }

    /// Add the imports of the automatic runtime's names the program used,
    /// once it is read. They make it a module; a script that needs them is
    /// a SyntaxError, which a caller unsure of the goal reads as "parse it
    /// as a module".
    pub(crate) fn jsx_finish_program(&mut self, body: &mut Vec<Stmt>) -> PResult<()> {
        let Some(st) = self.jsx.as_deref() else {
            return Ok(());
        };
        if !st.used.iter().any(|u| *u) {
            return Ok(());
        }
        if self.goal != Goal::Module {
            return Err(SyntaxError::new(
                "SyntaxError: JSX's automatic runtime is imported, so this file must be a module",
                0,
            ));
        }
        let source = st.source.clone().unwrap_or_else(|| "react".into());
        let runtime = format!(
            "{source}/{}",
            if st.dev {
                "jsx-dev-runtime"
            } else {
                "jsx-runtime"
            }
        );
        let used = st.used;
        let mut from_runtime = Vec::new();
        let mut from_source = Vec::new();
        for rt in Rt::ALL {
            if !used[rt as usize] {
                continue;
            }
            let spec = ImportSpecifier::Named {
                imported: ModuleExportName::Ident(self.intern_name(rt.export())),
                local: self.intern_name(rt.local()),
            };
            if matches!(rt, Rt::CreateElement) {
                from_source.push(spec);
            } else {
                from_runtime.push(spec);
            }
        }
        for (specifiers, module) in [(from_source, source), (from_runtime, runtime)] {
            if specifiers.is_empty() {
                continue;
            }
            body.insert(
                0,
                Stmt::Import(Box::new(ImportDecl {
                    specifiers,
                    source: StrVal::Utf8(module),
                    attributes: Vec::new(),
                    phase: ImportPhase::Evaluation,
                })),
            );
        }
        Ok(())
    }
}

fn str_expr(s: &str) -> Expr {
    Expr::Str(StrVal::Utf8(s.to_string()))
}

fn call(callee: Expr, args: Vec<Arg>, pos: u32) -> Expr {
    Expr::Call(Box::new(CallExpr {
        callee,
        args,
        optional: false,
        pos,
    }))
}

/// tsc's `hasKeyAfterPropsSpread`: a `key` after a spread that is not an
/// object literal of plain properties.
fn key_after_spread(attrs: &[Attr]) -> bool {
    let mut spread = false;
    for a in attrs {
        match a {
            Attr::Spread(Expr::Object(members, _))
                if !members.iter().any(|m| matches!(m, ObjectMember::Spread(_))) => {}
            Attr::Spread(_) => spread = true,
            Attr::Prop(name, _) if spread && name == "key" => return true,
            Attr::Prop(..) => {}
        }
    }
    false
}

/// `{...e}` into props: an object literal's own members inline (unless one
/// sets `__proto__`), as tsc writes it, else a spread.
fn push_spread(members: &mut Vec<ObjectMember>, e: Expr) {
    match e {
        Expr::Object(inner, _) if !inner.iter().any(sets_proto) => members.extend(inner),
        e => members.push(ObjectMember::Spread(e)),
    }
}

fn sets_proto(m: &ObjectMember) -> bool {
    match m {
        ObjectMember::Prop {
            key,
            shorthand: false,
            ..
        } => match key {
            PropKey::Ident(n) => &**n == "__proto__",
            PropKey::Str(s) => s.to_lossy_string() == "__proto__",
            _ => false,
        },
        _ => false,
    }
}

/// tsc's `fixupWhitespaceAndDecodeEntities`: a run of JSX text as the
/// string child it becomes, `None` when nothing is left of it.
fn jsx_text(raw: &str) -> Option<StrVal> {
    let line_break = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    let mut lines: Vec<&str> = Vec::new();
    // The first non-whitespace of the current line (`None` once a line
    // break has passed and none has followed), and the end of the last
    // non-whitespace seen.
    let mut first = Some(0);
    let mut last: Option<usize> = None;
    for (i, c) in raw.char_indices() {
        if line_break(c) {
            if let (Some(f), Some(l)) = (first, last) {
                if l > f {
                    lines.push(&raw[f..l]);
                }
            }
            first = None;
        } else if !single_line_space(c) {
            last = Some(i + c.len_utf8());
            if first.is_none() {
                first = Some(i);
            }
        }
    }
    if let Some(f) = first {
        lines.push(&raw[f..]);
    }
    if lines.is_empty() {
        return None;
    }
    Some(decode_entities(&lines.join(" ")))
}

/// tsc's `isWhiteSpaceSingleLine`.
fn single_line_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{b}' | '\u{c}' | '\u{a0}' | '\u{85}' | '\u{1680}' | '\u{2000}'
            ..='\u{200b}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `s` with its character references (`&amp;`, `&#169;`, `&#xA9;`)
/// decoded, as tsc's `decodeEntities` does: an unknown name stays as
/// written.
fn decode_entities(s: &str) -> StrVal {
    if !s.contains('&') {
        return StrVal::Utf8(s.to_string());
    }
    let mut out: Vec<u16> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with('&') {
            if let Some((cp, len)) = reference(rest) {
                match char::from_u32(cp) {
                    Some(c) => out.extend(c.encode_utf16(&mut [0; 2]).iter()),
                    // A lone surrogate (`&#xD800;`) is one code unit.
                    None => out.push(cp as u16),
                }
                i += len;
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.extend(c.encode_utf16(&mut [0; 2]).iter());
        i += c.len_utf8();
    }
    StrVal::from_utf16(out)
}

/// The character reference `s` starts with (`s` starts with `&`): its code
/// point and length.
fn reference(s: &str) -> Option<(u32, usize)> {
    let b = s.as_bytes();
    let run = |from: usize, f: fn(&u8) -> bool| {
        b[from.min(b.len())..].iter().take_while(|c| f(c)).count()
    };
    if b.get(1) == Some(&b'#') {
        if b.get(2) == Some(&b'x') {
            let n = run(3, u8::is_ascii_hexdigit);
            if n > 0 && b.get(3 + n) == Some(&b';') {
                let v = u32::from_str_radix(&s[3..3 + n], 16).ok()?;
                return (v <= 0x10FFFF).then_some((v, 4 + n));
            }
            return None;
        }
        let n = run(2, u8::is_ascii_digit);
        if n > 0 && b.get(2 + n) == Some(&b';') {
            let v: u32 = s[2..2 + n].parse().ok()?;
            return (v <= 0x10FFFF).then_some((v, 3 + n));
        }
        return None;
    }
    let n = run(1, |c| c.is_ascii_alphanumeric() || *c == b'_');
    if n > 0 && b.get(1 + n) == Some(&b';') {
        let name = &s[1..1 + n];
        let i = ENTITIES.binary_search_by(|(k, _)| (*k).cmp(name)).ok()?;
        return Some((ENTITIES[i].1 as u32, 2 + n));
    }
    None
}

/// The named character references JSX text decodes: XHTML 1.0's, as tsc
/// and Babel list them, sorted by name for a binary search.
const ENTITIES: [(&str, u16); 253] = [
    ("AElig", 0x00C6),
    ("Aacute", 0x00C1),
    ("Acirc", 0x00C2),
    ("Agrave", 0x00C0),
    ("Alpha", 0x0391),
    ("Aring", 0x00C5),
    ("Atilde", 0x00C3),
    ("Auml", 0x00C4),
    ("Beta", 0x0392),
    ("Ccedil", 0x00C7),
    ("Chi", 0x03A7),
    ("Dagger", 0x2021),
    ("Delta", 0x0394),
    ("ETH", 0x00D0),
    ("Eacute", 0x00C9),
    ("Ecirc", 0x00CA),
    ("Egrave", 0x00C8),
    ("Epsilon", 0x0395),
    ("Eta", 0x0397),
    ("Euml", 0x00CB),
    ("Gamma", 0x0393),
    ("Iacute", 0x00CD),
    ("Icirc", 0x00CE),
    ("Igrave", 0x00CC),
    ("Iota", 0x0399),
    ("Iuml", 0x00CF),
    ("Kappa", 0x039A),
    ("Lambda", 0x039B),
    ("Mu", 0x039C),
    ("Ntilde", 0x00D1),
    ("Nu", 0x039D),
    ("OElig", 0x0152),
    ("Oacute", 0x00D3),
    ("Ocirc", 0x00D4),
    ("Ograve", 0x00D2),
    ("Omega", 0x03A9),
    ("Omicron", 0x039F),
    ("Oslash", 0x00D8),
    ("Otilde", 0x00D5),
    ("Ouml", 0x00D6),
    ("Phi", 0x03A6),
    ("Pi", 0x03A0),
    ("Prime", 0x2033),
    ("Psi", 0x03A8),
    ("Rho", 0x03A1),
    ("Scaron", 0x0160),
    ("Sigma", 0x03A3),
    ("THORN", 0x00DE),
    ("Tau", 0x03A4),
    ("Theta", 0x0398),
    ("Uacute", 0x00DA),
    ("Ucirc", 0x00DB),
    ("Ugrave", 0x00D9),
    ("Upsilon", 0x03A5),
    ("Uuml", 0x00DC),
    ("Xi", 0x039E),
    ("Yacute", 0x00DD),
    ("Yuml", 0x0178),
    ("Zeta", 0x0396),
    ("aacute", 0x00E1),
    ("acirc", 0x00E2),
    ("acute", 0x00B4),
    ("aelig", 0x00E6),
    ("agrave", 0x00E0),
    ("alefsym", 0x2135),
    ("alpha", 0x03B1),
    ("amp", 0x0026),
    ("and", 0x2227),
    ("ang", 0x2220),
    ("apos", 0x0027),
    ("aring", 0x00E5),
    ("asymp", 0x2248),
    ("atilde", 0x00E3),
    ("auml", 0x00E4),
    ("bdquo", 0x201E),
    ("beta", 0x03B2),
    ("brvbar", 0x00A6),
    ("bull", 0x2022),
    ("cap", 0x2229),
    ("ccedil", 0x00E7),
    ("cedil", 0x00B8),
    ("cent", 0x00A2),
    ("chi", 0x03C7),
    ("circ", 0x02C6),
    ("clubs", 0x2663),
    ("cong", 0x2245),
    ("copy", 0x00A9),
    ("crarr", 0x21B5),
    ("cup", 0x222A),
    ("curren", 0x00A4),
    ("dArr", 0x21D3),
    ("dagger", 0x2020),
    ("darr", 0x2193),
    ("deg", 0x00B0),
    ("delta", 0x03B4),
    ("diams", 0x2666),
    ("divide", 0x00F7),
    ("eacute", 0x00E9),
    ("ecirc", 0x00EA),
    ("egrave", 0x00E8),
    ("empty", 0x2205),
    ("emsp", 0x2003),
    ("ensp", 0x2002),
    ("epsilon", 0x03B5),
    ("equiv", 0x2261),
    ("eta", 0x03B7),
    ("eth", 0x00F0),
    ("euml", 0x00EB),
    ("euro", 0x20AC),
    ("exist", 0x2203),
    ("fnof", 0x0192),
    ("forall", 0x2200),
    ("frac12", 0x00BD),
    ("frac14", 0x00BC),
    ("frac34", 0x00BE),
    ("frasl", 0x2044),
    ("gamma", 0x03B3),
    ("ge", 0x2265),
    ("gt", 0x003E),
    ("hArr", 0x21D4),
    ("harr", 0x2194),
    ("hearts", 0x2665),
    ("hellip", 0x2026),
    ("iacute", 0x00ED),
    ("icirc", 0x00EE),
    ("iexcl", 0x00A1),
    ("igrave", 0x00EC),
    ("image", 0x2111),
    ("infin", 0x221E),
    ("int", 0x222B),
    ("iota", 0x03B9),
    ("iquest", 0x00BF),
    ("isin", 0x2208),
    ("iuml", 0x00EF),
    ("kappa", 0x03BA),
    ("lArr", 0x21D0),
    ("lambda", 0x03BB),
    ("lang", 0x2329),
    ("laquo", 0x00AB),
    ("larr", 0x2190),
    ("lceil", 0x2308),
    ("ldquo", 0x201C),
    ("le", 0x2264),
    ("lfloor", 0x230A),
    ("lowast", 0x2217),
    ("loz", 0x25CA),
    ("lrm", 0x200E),
    ("lsaquo", 0x2039),
    ("lsquo", 0x2018),
    ("lt", 0x003C),
    ("macr", 0x00AF),
    ("mdash", 0x2014),
    ("micro", 0x00B5),
    ("middot", 0x00B7),
    ("minus", 0x2212),
    ("mu", 0x03BC),
    ("nabla", 0x2207),
    ("nbsp", 0x00A0),
    ("ndash", 0x2013),
    ("ne", 0x2260),
    ("ni", 0x220B),
    ("not", 0x00AC),
    ("notin", 0x2209),
    ("nsub", 0x2284),
    ("ntilde", 0x00F1),
    ("nu", 0x03BD),
    ("oacute", 0x00F3),
    ("ocirc", 0x00F4),
    ("oelig", 0x0153),
    ("ograve", 0x00F2),
    ("oline", 0x203E),
    ("omega", 0x03C9),
    ("omicron", 0x03BF),
    ("oplus", 0x2295),
    ("or", 0x2228),
    ("ordf", 0x00AA),
    ("ordm", 0x00BA),
    ("oslash", 0x00F8),
    ("otilde", 0x00F5),
    ("otimes", 0x2297),
    ("ouml", 0x00F6),
    ("para", 0x00B6),
    ("part", 0x2202),
    ("permil", 0x2030),
    ("perp", 0x22A5),
    ("phi", 0x03C6),
    ("pi", 0x03C0),
    ("piv", 0x03D6),
    ("plusmn", 0x00B1),
    ("pound", 0x00A3),
    ("prime", 0x2032),
    ("prod", 0x220F),
    ("prop", 0x221D),
    ("psi", 0x03C8),
    ("quot", 0x0022),
    ("rArr", 0x21D2),
    ("radic", 0x221A),
    ("rang", 0x232A),
    ("raquo", 0x00BB),
    ("rarr", 0x2192),
    ("rceil", 0x2309),
    ("rdquo", 0x201D),
    ("real", 0x211C),
    ("reg", 0x00AE),
    ("rfloor", 0x230B),
    ("rho", 0x03C1),
    ("rlm", 0x200F),
    ("rsaquo", 0x203A),
    ("rsquo", 0x2019),
    ("sbquo", 0x201A),
    ("scaron", 0x0161),
    ("sdot", 0x22C5),
    ("sect", 0x00A7),
    ("shy", 0x00AD),
    ("sigma", 0x03C3),
    ("sigmaf", 0x03C2),
    ("sim", 0x223C),
    ("spades", 0x2660),
    ("sub", 0x2282),
    ("sube", 0x2286),
    ("sum", 0x2211),
    ("sup", 0x2283),
    ("sup1", 0x00B9),
    ("sup2", 0x00B2),
    ("sup3", 0x00B3),
    ("supe", 0x2287),
    ("szlig", 0x00DF),
    ("tau", 0x03C4),
    ("there4", 0x2234),
    ("theta", 0x03B8),
    ("thetasym", 0x03D1),
    ("thinsp", 0x2009),
    ("thorn", 0x00FE),
    ("tilde", 0x02DC),
    ("times", 0x00D7),
    ("trade", 0x2122),
    ("uArr", 0x21D1),
    ("uacute", 0x00FA),
    ("uarr", 0x2191),
    ("ucirc", 0x00FB),
    ("ugrave", 0x00F9),
    ("uml", 0x00A8),
    ("upsih", 0x03D2),
    ("upsilon", 0x03C5),
    ("uuml", 0x00FC),
    ("weierp", 0x2118),
    ("xi", 0x03BE),
    ("yacute", 0x00FD),
    ("yen", 0x00A5),
    ("yuml", 0x00FF),
    ("zeta", 0x03B6),
    ("zwj", 0x200D),
    ("zwnj", 0x200C),
];
