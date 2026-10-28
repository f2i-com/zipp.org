//! JSX in `.tsx` and `.jsx` files, lowered as tsc lowers it. The runtime here
//! records each call (which function, the element type, the props as it
//! received them, the arity and the key), so the expected lines pin the
//! lowering itself, not only what a renderer would make of it. They are
//! what Node prints for tsc's output of the same files (`transpileModule`
//! with each directory's `tsconfig.json`).
#![cfg(feature = "typescript")]

use std::path::Path;

const RUNTIME: &str = r#"export function Fragment() {}
const name = (t) => (typeof t === "string" ? t : t.name);
const ser = (v) =>
  typeof v === "function" ? "fn" : Array.isArray(v) ? v.map(ser) : v && typeof v === "object" && !v.$ ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, ser(x)])) : v;
const rec = (f) => function (type, props, key) { return { $: f, type: name(type), props: ser(props), n: arguments.length, ...(arguments.length > 2 ? { key } : {}) }; };
export const jsx = rec("jsx");
export const jsxs = rec("jsxs");
export function createElement(type, props, ...children) { return { $: "ce", type: name(type), props: ser(props), children: ser(children) }; }
"#;

const FILES: &[(&str, &str)] = &[
    ("node_modules/rec/index.mjs", RUNTIME),
    (
        "node_modules/rec/package.json",
        r#"{ "name": "rec", "type": "module", "exports": { ".": "./index.mjs", "./jsx-runtime": "./index.mjs" } }"#,
    ),
    // `react` stands in for the default import source.
    ("node_modules/react/index.mjs", RUNTIME),
    (
        "node_modules/react/package.json",
        r#"{ "name": "react", "type": "module", "exports": { ".": "./index.mjs", "./jsx-runtime": "./index.mjs" } }"#,
    ),
    (
        "auto/tsconfig.json",
        r#"{ "compilerOptions": { "jsx": "react-jsx", "jsxImportSource": "rec" } }"#,
    ),
    (
        "auto/main.tsx",
        r#"import type { Props } from "./types";
const show = (el: unknown) => console.log(JSON.stringify(el));
const Item = ({ name }: Props) => name;
const items = ["a", "b"];
const p = { x: 1 };
const id = <T,>(x: T) => x;
show(<div className="c" data-x="1" hidden />);
show(<p>
  Hello,   world &amp; &copy;
  {/* nothing */}
</p>);
show(<ul>{items.map((n) => <Item key={n} name={n} />)}</ul>);
show(<li {...p} key="k" />);
show(<li key="k" {...p} />);
show(<><b />text{id(1)}</>);
show(<Item<string> name="typed" />);
show(<svg:rect xlink:href="/a" aria-label='it&apos;s' />);
"#,
    ),
    (
        "classic/tsconfig.json",
        r#"{ "compilerOptions": { "jsx": "react", "jsxFactory": "R.createElement", "jsxFragmentFactory": "R.Fragment" } }"#,
    ),
    // `R` is used only by the elements: the import must not be elided.
    (
        "classic/main.tsx",
        r#"import * as R from "rec";
const show = (el: unknown) => console.log(JSON.stringify(el));
const NS = { Inner: function Inner() {} };
show(<div />);
show(<NS.Inner a={1} {...{ b: 2 }}>x {"y"} <i /></NS.Inner>);
show(<>{...[1, 2]}</>);
"#,
    ),
    // No `tsconfig.json`: React's automatic runtime, and no import or export
    // of its own, yet a module.
    (
        "default/main.tsx",
        r#"console.log(JSON.stringify(<p>{"x"}</p>));
"#,
    ),
    // JavaScript with JSX: no TypeScript grammar (`a < b > (0)` compares),
    // and pragmas choose the classic factory.
    (
        "plain/main.jsx",
        r#"/** @jsx h */
/** @jsxFrag Frag */
const h = (t, p, ...c) => [typeof t === "string" ? t : t.name, p, c];
function Frag() {}
const a = 1, b = 2;
console.log(JSON.stringify([<div id="x">hi {a < b}</div>, <>f</>, a < b > (0)]));
"#,
    ),
];

fn run(path: &Path) -> Vec<String> {
    zipp_vm::set_node_runtime(
        true,
        vec!["zipp".into(), path.to_string_lossy().into_owned()],
    );
    let out = zipp_vm::frontend::run_typescript_file(path).expect("runs");
    assert_eq!(out.error, None, "{:?}", out.errput);
    out.output
        .iter()
        .flat_map(|l| l.lines().map(str::to_string))
        .collect()
}

#[test]
fn jsx_lowers_as_tsc_lowers_it() {
    let root = std::env::temp_dir().join(format!("zipp-jsx-{}", std::process::id()));
    for (rel, text) in FILES {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    assert_eq!(
        run(&root.join("auto/main.tsx")),
        [
            r#"{"$":"jsx","type":"div","props":{"className":"c","data-x":"1","hidden":true},"n":2}"#,
            r#"{"$":"jsx","type":"p","props":{"children":"Hello,   world & ©"},"n":2}"#,
            r#"{"$":"jsx","type":"ul","props":{"children":[{"$":"jsx","type":"Item","props":{"name":"a"},"n":3,"key":"a"},{"$":"jsx","type":"Item","props":{"name":"b"},"n":3,"key":"b"}]},"n":2}"#,
            r#"{"$":"ce","type":"li","props":{"x":1,"key":"k"},"children":[]}"#,
            r#"{"$":"jsx","type":"li","props":{"x":1},"n":3,"key":"k"}"#,
            r#"{"$":"jsxs","type":"Fragment","props":{"children":[{"$":"jsx","type":"b","props":{},"n":2},"text",1]},"n":2}"#,
            r#"{"$":"jsx","type":"Item","props":{"name":"typed"},"n":2}"#,
            r#"{"$":"jsx","type":"svg:rect","props":{"xlink:href":"/a","aria-label":"it's"},"n":2}"#,
        ]
    );
    assert_eq!(
        run(&root.join("classic/main.tsx")),
        [
            r#"{"$":"ce","type":"div","props":null,"children":[]}"#,
            r#"{"$":"ce","type":"Inner","props":{"a":1,"b":2},"children":["x ","y"," ",{"$":"ce","type":"i","props":null,"children":[]}]}"#,
            r#"{"$":"ce","type":"Fragment","props":null,"children":[1,2]}"#,
        ]
    );
    assert_eq!(
        run(&root.join("default/main.tsx")),
        [r#"{"$":"jsx","type":"p","props":{"children":"x"},"n":2}"#]
    );
    assert_eq!(
        run(&root.join("plain/main.jsx")),
        [r#"[["div",{"id":"x"},["hi ",true]],["Frag",null,["f"]],true]"#]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// What is not JSX is a SyntaxError where it is.
#[test]
fn malformed_jsx_is_a_syntax_error() {
    let root = std::env::temp_dir().join(format!("zipp-jsx-bad-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for (src, want) in [
        (
            "<div></span>",
            "expected corresponding JSX closing tag for 'div'",
        ),
        ("<div>a > b</div>", "unexpected '>' in JSX text"),
        (
            "<a b={} />",
            "JSX attributes must only be assigned a non-empty expression",
        ),
        ("<a>x", "<a> has no closing tag"),
        // In a `.tsx` file `<T>` starts an element, not a type assertion.
        ("const f = <T>(y) => y;", "unexpected '>' in JSX text"),
    ] {
        let p = root.join("bad.tsx");
        std::fs::write(&p, src).unwrap();
        let err = zipp_vm::frontend::check_typescript_file(&p, src).unwrap_err();
        assert!(err.contains(want), "{src}: {err}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
