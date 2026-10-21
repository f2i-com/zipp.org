//! `Error.prototype.stack` in V8's format (#78). Every expected position is
//! the one Node 23 reports for the same source; the frames differ only where
//! V8 prefixes a method with its receiver's type (`A.m`, `Object.top [as g]`)
//! and where Node's module wrapper names the top level `Object.<anonymous>`
//! (V8 itself, as in Chrome and Deno, prints the bare location).

use zipp_vm::embed::{compile_script, ScriptState};

fn run(src: &str, name: Option<&str>) -> Vec<String> {
    let mut st: ScriptState = compile_script(src).expect("compiles");
    if let Some(name) = name {
        st.set_script_name(name);
    }
    st.run_init().expect("runs");
    st.take_output()
}

#[test]
fn the_issue_example() {
    let out = run(
        "function inner() { return new Error(\"boom\").stack; }\n\
         function outer() { return inner(); }\n\
         console.log(JSON.stringify(outer()));\n",
        Some("file.js"),
    );
    assert_eq!(
        out,
        [r#""Error: boom\n    at inner (file.js:1:27)\n    at outer (file.js:2:27)\n    at file.js:3:28""#]
    );
}

#[test]
fn call_shapes_report_v8_positions() {
    let out = run(
        r#"function top(){ return new Error("e").stack.split("\n").slice(1, 3).join(" | "); }
const o = { g: top, h: { k: top } };
console.log(o.g());
console.log(  o.h.k  ());
console.log(o["g"]());
console.log((0, o.g)());
console.log(`${top()}`);
function C() { this.s = new Error().stack.split("\n").slice(1, 3).join(" | "); }
console.log(new C().s);
const arrow = () => new Error().stack.split("\n")[1];
console.log(arrow());
function notFirst() { let a = [1, 2].map(x => x * 2); let b = a.length + 1; return new Error("later").stack.split("\n")[1]; }
console.log(notFirst());
"#,
        Some("t.js"),
    );
    assert_eq!(
        out,
        [
            "    at top (t.js:1:24) |     at t.js:3:15",
            "    at top (t.js:1:24) |     at t.js:4:19",
            "    at top (t.js:1:24) |     at t.js:5:19",
            "    at top (t.js:1:24) |     at t.js:6:21",
            "    at top (t.js:1:24) |     at t.js:7:16",
            "    at new C (t.js:8:25) |     at t.js:9:13",
            "    at arrow (t.js:10:21)",
            "    at notFirst (t.js:12:84)",
        ]
    );
}

#[test]
fn a_subclass_stack_starts_at_its_construction() {
    let out = run(
        r#"class MyErr extends Error { constructor(m) { super(m); this.name = "MyErr"; } }
function thrower() { throw new MyErr("custom"); }
try { thrower(); } catch (e) { console.log(e.stack.split("\n").join(" | ")); }
class B2 extends Error {} class C2 extends B2 { constructor() { super("deep"); } }
function mk2() { return new C2(); }
console.log(mk2().stack.split("\n").join(" | "));
"#,
        Some("s.js"),
    );
    assert_eq!(
        out,
        [
            "MyErr: custom |     at thrower (s.js:2:28) |     at s.js:3:7",
            "Error: deep |     at mk2 (s.js:5:25) |     at s.js:6:13",
        ]
    );
}

#[test]
fn the_header_is_read_once_and_the_setter_still_replaces() {
    let out = run(
        r#"const e = new Error("a"); e.message = "b";
console.log(e.stack.split("\n")[0]); e.message = "c"; console.log(e.stack.split("\n")[0]);
const x = new Error("x"); x.stack = "replaced"; console.log(x.stack);
console.log(typeof new TypeError("t").stack, new RangeError("r").stack.split("\n")[0]);
console.log(Object.getOwnPropertyDescriptor(new Error(), "stack") === undefined);
function deep(n) { if (n === 0) return new Error().stack.split("\n").length; return deep(n - 1); }
console.log(deep(30));
"#,
        None,
    );
    assert_eq!(out, ["Error: b", "Error: b", "replaced", "string RangeError: r", "true", "11"]);
}

#[test]
fn unnamed_scripts_eval_code_and_engine_errors() {
    let out = run(
        r#"console.log(new Error("top").stack);
console.log(eval("new Error('in eval').stack").split("\n").join(" | "));
console.log(new Function("return new Error('in fn').stack")().split("\n").join(" | "));
try { null.x; } catch (e) { console.log(e.stack.split("\n").slice(1).join(" | ")); }
"#,
        None,
    );
    assert_eq!(
        out,
        [
            "Error: top\n    at <anonymous>:1:13",
            "Error: in eval |     at <anonymous>:1:1 |     at <anonymous>:2:13",
            "Error: in fn |     at anonymous (<anonymous>:3:8) |     at <anonymous>:3:60",
            "    at <anonymous>",
        ]
    );
}

#[test]
fn hot_functions_keep_their_frames() {
    let out = run(
        r#"function mk(i) { return new Error("m" + i); }
let last; for (let i = 0; i < 20000; i++) last = mk(i);
console.log(last.stack.split("\n").join(" | "));
"#,
        Some("h.js"),
    );
    assert_eq!(out, ["Error: m19999 |     at mk (h.js:1:25) |     at h.js:2:50"]);
}

#[cfg(not(feature = "wasm-no-fs-loader"))]
#[test]
fn module_frames_name_their_file_url() {
    let dir = std::env::temp_dir().join(format!("zipp-error-stack-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the module directory");
    std::fs::write(
        dir.join("main.mjs"),
        "import { make } from './dep.mjs';\nconsole.log(make().split('\\n').join(' | '));\n",
    )
    .expect("write main.mjs");
    std::fs::write(dir.join("dep.mjs"), "export function make() { return new Error('from dep').stack; }\n")
        .expect("write dep.mjs");
    let path = dir.join("main.mjs");
    let out = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || zipp_vm::run_module_file(&path, None))
        .expect("spawn")
        .join()
        .expect("join")
        .expect("the module runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.error.is_none(), "error: {:?}", out.error);
    let line = &out.output[0];
    let parts: Vec<&str> = line.split(" | ").collect();
    assert_eq!(parts[0], "Error: from dep");
    assert!(parts[1].starts_with("    at make (file://") && parts[1].ends_with("/dep.mjs:1:33)"), "{line}");
    assert!(parts[2].starts_with("    at file://") && parts[2].ends_with("/main.mjs:2:13"), "{line}");
}
