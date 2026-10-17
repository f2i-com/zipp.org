//! A built-in constructor's own data properties (`Object.hasOwn`,
//! `Array.isArray`, `Number.MAX_SAFE_INTEGER`) are read straight from its
//! map, and a native captured by a spec-order member call is called
//! directly. Every read must still match the general walk: the synthesized
//! `prototype`, `name` and `length`, an assigned static, a deleted one, one
//! redefined as an accessor or made non-writable, and statics of every
//! built-in constructor; and every call its native, with side-effecting
//! arguments, a throwing native and natives whose `this` matters. The
//! expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
log("statics", typeof Object.hasOwn, typeof Array.isArray, Number.MAX_SAFE_INTEGER, Number.EPSILON > 0, typeof String.fromCharCode, typeof Promise.resolve, typeof Symbol.iterator, typeof Date.now, typeof BigInt.asIntN);
log("synthesized", Object.prototype === Object.getPrototypeOf({}), Object.name, Object.length, Array.name, Array.length, typeof Array.prototype.push);
Object.myStatic = 5; Array.tag = "t";
log("assigned", Object.myStatic, Array.tag);
var savedFrom = Array.from;
delete Array.from;
log("deleted", typeof Array.from);
Array.from = savedFrom;
log("restored", typeof Array.from);
Object.defineProperty(Number, "gotten", { get: function () { return "via getter"; }, configurable: true });
log("accessor", Number.gotten);
Object.defineProperty(Object, "fixed", { value: 42, writable: false, configurable: true });
Object.fixed = 1;
log("non-writable", Object.fixed);
var saved = Object.keys;
Object.keys = function () { return ["replaced"]; };
log("replaced", Object.keys({ a: 1 }).join());
Object.keys = saved;
var keys = ["a", "b", "zz"], objs = [{ a: 1 }, { b: 2 }, { zz: 3 }], hits = 0, sideEffects = 0;
function k(i) { sideEffects++; return keys[i % 3]; }
for (var i = 0; i < 30; i++) { if (Object.hasOwn(objs[i % 3], k(i))) hits++; if (Array.isArray([objs[i % 3]][0])) hits += 100; }
log("loop", hits, sideEffects);
log("split natives", String.fromCharCode([72, 105][0], [72, 105][1]), JSON.stringify(objs[0]), Object.keys(objs[2]).join(), Number.isInteger([1.5, 2][1]), Object.getPrototypeOf(objs[1]) === Object.prototype);
attempt("throwing native", function () { return Object.defineProperty([1][1], "x", {}); });
attempt("hasOwn null", function () { return Object.hasOwn([null][0], "a"); });
log("this natives", Array.prototype.slice.call([1, 2, 3], [1][0]).join(), Promise.resolve([7][0]) instanceof Promise);
class Base { static make() { return "made"; } }
log("class static", Base.make(), typeof Base.prototype);
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn ctor_static_reads_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
