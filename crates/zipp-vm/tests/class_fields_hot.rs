//! Class fields on compiled code, and a loop region calling its own function.
//!
//! A constructor or field initializer holding a `DefineField` used to be left
//! uncompiled, and a class that declares its fields (every TypeScript class
//! with parameter properties under ES2022 settings) constructed several times
//! slower than one assigning in its constructor. `DefineField` now compiles
//! (`jit_define_field`, the interpreter's own append or a decline), a base
//! class's fields run inline in its constructor, and the store-only
//! constructor path takes a body that defines fields first. Each case below
//! runs 30,000 times so those paths are hot, then prints what a program can
//! see: a prototype setter not run, a non-extensible `this`, a field defined
//! twice, a non-configurable property, derived classes, key order,
//! `new.target` in an initializer, a getter-only accessor, a class with no
//! constructor, a return override and a frozen instance. The expected lines
//! are Node 24's.
//!
//! The second program is a recursive arrow that concatenates arrays in a loop:
//! its loop region baked a native call to the function's own entry, a lane
//! meant only for a whole-function compile, and crashed once the function's
//! body compiled.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const FIELDS: &str = r####"const N = 30000;
function run(name, f) {
  let last;
  try { for (let i = 0; i < N; i++) last = f(i); console.log(name, JSON.stringify(last)); }
  catch (e) { console.log(name, "threw", e.constructor.name); }
}
// 1. a setter on the prototype must not run for a field
let setterRuns = 0;
class P1 { x = 1; constructor(v) { this.y = v; } }
Object.defineProperty(P1.prototype, "x", { set(v) { setterRuns++; }, get() { return "proto"; }, configurable: true });
run("proto-setter", (i) => { const o = new P1(i); return [o.x, o.y, setterRuns, Object.keys(o)]; });
// 2. a non-extensible this: the next field throws
class P2 { a = Object.preventExtensions(this); b = 1; constructor() {} }
run("non-extensible", () => new P2());
// 3. the same field twice
class P3 { x = 1; x = 2; constructor(z) { this.z = z; } }
run("twice", (i) => { const o = new P3(i); return [o.x, Object.keys(o)]; });
// 4. a non-configurable property made by an earlier initializer
class P4 { y = Object.defineProperty(this, "z", { value: 1, configurable: false }); z = 2; constructor() {} }
run("non-configurable", () => new P4());
// 5. derived classes: fields after super()
class B5 { base = "b"; constructor(v) { this.v = v; } }
class D5 extends B5 { d = this.v * 2; e; constructor(v) { super(v); this.f = 1; } }
run("derived", (i) => { const o = new D5(i); return [Object.keys(o), o.d]; });
// 6. key order with parameter-property shape (declared, then assigned)
class P6 { a; b; c = 3; constructor(a, b) { this.b = b; this.a = a; this.extra = a + b; } }
run("order", (i) => { const o = new P6(i, 1); return [Object.keys(o), o.a, o.b, o.c, o.extra]; });
// 7. new.target in a field initializer of a class without a constructor
class P7 { t = new.target; u = 1; }
run("new.target", () => { const o = new P7(); return [o.t === undefined, o.u]; });
// 8. a getter-only accessor on the prototype: the field still defines
class P8 { k = 5; }
Object.defineProperty(P8.prototype, "k", { get() { return 0; } });
run("getter-only", () => { const o = new P8(); return [o.k, Object.getOwnPropertyDescriptor(o, "k")]; });
// 9. fields without a constructor, arrays and objects as initializers
class P9 { list = []; map = { n: 1 }; count = 0; push(v) { this.list.push(v); this.count++; return this; } }
run("implicit", (i) => { const o = new P9().push(i).push(i + 1); return [o.count, o.list.length, o.map.n]; });
// 10. a return override in the constructor
class P10 { x = 1; constructor(r) { if (r) return { over: true }; } }
run("override", (i) => [new P10(i % 2).over, new P10(0).x]);
// 11. Object.freeze inside the constructor after fields
class P11 { x = 1; constructor() { Object.freeze(this); } }
run("frozen", () => { const o = new P11(); try { o.x = 2; } catch (e) { return "threw"; } return o.x; });
"####;

const RECURSIVE_REGION: &str = r####"const parse = (text, open = "<", close = ">") => {
  let array = []; let floor = 0; let begin = 0, end = 0;
  for (let i = 0; i < text.length; i++) {
    if (text[i] === open) { floor++; if (floor === 1) begin = i; }
    else if (text[i] === close) {
      if (floor === 1) { end = i; const tag = text.slice(begin, end + 1); array.push(tag); array = array.concat(parse(tag.slice(1, -1), open, close)); }
      floor--;
    }
  }
  return array;
};
console.log(JSON.stringify(parse("THIS IS SAMPLE TEXT(MAIN hoge 0.1 fuga(ITEM fuga hoge)hoge(ITEM2 nogami(ABBR)))", "(", ")")));
"####;

#[test]
fn class_fields_on_hot_paths_match_node() {
    assert_eq!(
        run(FIELDS),
        [
            r#"proto-setter [1,29999,0,["x","y"]]"#,
            "non-extensible threw TypeError",
            r#"twice [2,["x","z"]]"#,
            "non-configurable threw TypeError",
            r#"derived [["base","v","d","e","f"],59998]"#,
            r#"order [["a","b","c","extra"],29999,1,3,30000]"#,
            "new.target [true,1]",
            r#"getter-only [5,{"value":5,"writable":true,"enumerable":true,"configurable":true}]"#,
            "implicit [2,2,1]",
            "override [true,1]",
            "frozen 1"
        ]
    );
}

#[test]
fn a_loop_region_calling_its_own_function_runs() {
    assert_eq!(
        run(RECURSIVE_REGION),
        [
            r#"["(MAIN hoge 0.1 fuga(ITEM fuga hoge)hoge(ITEM2 nogami(ABBR)))","(ITEM fuga hoge)","(ITEM2 nogami(ABBR))","(ABBR)"]"#
        ]
    );
}
