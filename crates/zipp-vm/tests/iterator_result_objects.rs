//! Iterator result objects built from a shared key plan.
//!
//! `Vm::iter_result` builds every `{value, done}` from one key plan the VM
//! keeps, as an object literal is built. The objects must stay ordinary
//! objects in every respect: their keys and order, JSON, `for-in`, a property
//! added, deleted or redefined on one, extensibility, and the results of
//! generators, Map, Set and Array iterators, `matchAll` and async generators.
//!
//! The expected line is Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"function* g() { yield 1; return 2; }
var it = g(), a = it.next(), b = it.next(), c = it.next();
var out = [JSON.stringify([a, b, c]), Object.keys(a).join(","), Object.getOwnPropertyNames(b).join(",")];
a.extra = 5; delete a.value; out.push(JSON.stringify(a), Object.keys(a).join(","));
Object.defineProperty(b, "done", { value: "x", enumerable: false }); out.push(JSON.stringify(b), JSON.stringify(Object.getOwnPropertyDescriptor(b, "done")));
var keys = []; for (var k in c) keys.push(k); out.push(keys.join(","));
var m = new Map([[1, "a"]]).entries().next(); out.push(JSON.stringify(m), Object.isFrozen(m), Object.isExtensible(m));
var s = new Set([3]).values(); out.push(JSON.stringify(s.next()), JSON.stringify(s.next()));
var ai = [7, 8][Symbol.iterator](); out.push(JSON.stringify(ai.next()));
var r = "ab".matchAll(/a/g).next(); out.push(r.done, r.value[0]);
async function* ag() { yield 1; } ag().next().then(function (v) { out.push(JSON.stringify(v), Object.keys(v).join(",")); console.log(out.join(" | ")); });
"####;

#[test]
fn iterator_result_objects_match_node() {
    let expected: &[&str] = &[
        r####"[{"value":1,"done":false},{"value":2,"done":true},{"done":true}] | value,done | value,done | {"done":false,"extra":5} | done,extra | {"value":2} | {"value":"x","writable":true,"enumerable":false,"configurable":true} | value,done | {"value":[1,"a"],"done":false} | false | true | {"value":3,"done":false} | {"done":true} | {"value":7,"done":false} | false | a | {"value":1,"done":false} | value,done"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
