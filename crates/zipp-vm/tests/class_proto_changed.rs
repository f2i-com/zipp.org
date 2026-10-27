//! Inherited reads on class instances after a prototype is changed.
//!
//! An instance's inherited members resolve through its classes' member
//! tables, a cache of their prototypes. Once a farther class's prototype is
//! changed (`Object.defineProperty(A.prototype, "m", …)`, a replaced or
//! deleted method) the read resumes at the nearest prototype object that
//! exists; when a nearer class's prototype had never been made, that skipped
//! its `constructor`, which no table holds, and `c.constructor` named the
//! farther class. Each case mutates a level of a three-class chain before or
//! after the reads are hot, then reads `constructor`, own and inherited
//! methods through the instances. The expected lines are Node 24's.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state
        .take_output()
        .iter()
        .flat_map(|chunk| chunk.lines().map(str::to_string).collect::<Vec<_>>())
        .collect()
}

const PROGRAM: &str = r####"const out = [];
const log = (s) => out.push(String(s));
const N = 3000;
function hot(o) { let s = ""; for (let i = 0; i < N; i++) s = o.constructor.name + "/" + o.kind() + "/" + o.base(); return s; }
function mk() {
  class A { base() { return "A.base"; } kind() { return "A"; } who() { return this.constructor.name; } }
  class B extends A { kind() { return "B"; } }
  class C extends B {}
  return { A, B, C };
}
// Mutation of each level, before and after instances exist and reads are hot.
for (const [label, mutate] of [
  ["redefine A.method", ({ A }) => Object.defineProperty(A.prototype, "base", Object.getOwnPropertyDescriptor(A.prototype, "base"))],
  ["replace A.method", ({ A }) => { A.prototype.base = function () { return "A.base2"; }; }],
  ["delete B.kind", ({ B }) => { delete B.prototype.kind; }],
  ["redefine B.kind", ({ B }) => Object.defineProperty(B.prototype, "kind", { value() { return "B2"; }, configurable: true, writable: true })],
  ["A.constructor", ({ A }) => { A.prototype.constructor = function Fake() {}; }],
  ["B.constructor getter", ({ B }) => Object.defineProperty(B.prototype, "constructor", { get() { return { name: "Getter" }; }, configurable: true })],
  ["setPrototypeOf C.prototype", ({ C, A }) => Object.setPrototypeOf(C.prototype, A.prototype)],
]) {
  for (const when of ["before", "after"]) {
    const k = mk();
    let first = "";
    if (when === "before") mutate(k);
    const c = new k.C(), b = new k.B(), a = new k.A();
    if (when === "after") { first = hot(c) + "|" + hot(b) + "|" + c.who(); mutate(k); }
    log(`${label} ${when}: ${first} -> ${hot(c)} | ${hot(b)} | ${hot(a)} | ${c.who()} ${b.who()} | ${Object.getPrototypeOf(c).constructor.name}`);
  }
}
console.log(out.join("\n"));
"####;

#[test]
fn constructor_reads_follow_a_changed_chain_as_node_does() {
    assert_eq!(
        run(PROGRAM),
        [
            r#"redefine A.method before:  -> C/B/A.base | B/B/A.base | A/A/A.base | C B | C"#,
            r#"redefine A.method after: C/B/A.base|B/B/A.base|C -> C/B/A.base | B/B/A.base | A/A/A.base | C B | C"#,
            r#"replace A.method before:  -> C/B/A.base2 | B/B/A.base2 | A/A/A.base2 | C B | C"#,
            r#"replace A.method after: C/B/A.base|B/B/A.base|C -> C/B/A.base2 | B/B/A.base2 | A/A/A.base2 | C B | C"#,
            r#"delete B.kind before:  -> C/A/A.base | B/A/A.base | A/A/A.base | C B | C"#,
            r#"delete B.kind after: C/B/A.base|B/B/A.base|C -> C/A/A.base | B/A/A.base | A/A/A.base | C B | C"#,
            r#"redefine B.kind before:  -> C/B2/A.base | B/B2/A.base | A/A/A.base | C B | C"#,
            r#"redefine B.kind after: C/B/A.base|B/B/A.base|C -> C/B2/A.base | B/B2/A.base | A/A/A.base | C B | C"#,
            r#"A.constructor before:  -> C/B/A.base | B/B/A.base | Fake/A/A.base | C B | C"#,
            r#"A.constructor after: C/B/A.base|B/B/A.base|C -> C/B/A.base | B/B/A.base | Fake/A/A.base | C B | C"#,
            r#"B.constructor getter before:  -> C/B/A.base | Getter/B/A.base | A/A/A.base | C Getter | C"#,
            r#"B.constructor getter after: C/B/A.base|B/B/A.base|C -> C/B/A.base | Getter/B/A.base | A/A/A.base | C Getter | C"#,
            r#"setPrototypeOf C.prototype before:  -> C/A/A.base | B/B/A.base | A/A/A.base | C B | C"#,
            r#"setPrototypeOf C.prototype after: C/B/A.base|B/B/A.base|C -> C/A/A.base | B/B/A.base | A/A/A.base | C B | C"#,
        ]
    );
}
