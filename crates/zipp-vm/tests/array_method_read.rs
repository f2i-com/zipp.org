//! An Array's method, read by a member call whose arguments are evaluated
//! after it.
//!
//! `stack.push({ node, parent })` and `list.includes(xs[i])` read the method
//! with a property read of their own, before the arguments. Compiled code
//! holds an Array in that read's cache by identity, so a site that sees a
//! new array each time called the miss helper on every read. It now loads
//! the method from its live slot of `%Array.prototype%`
//! (`emit_inline_arr_proto_prop`) for a plain Array whose prototype is that
//! object, while the prototype's layout is the one the body compiled against.
//!
//! The first program runs those sites over arrays, then over every other
//! kind of receiver the same sites can see (a string, a subclass instance,
//! an array with its own method, one with another prototype or none, a
//! plain object, a typed array, an `arguments` object, a Proxy), then
//! replaces, deletes, redefines as an accessor and adds methods on
//! `%Array.prototype%` while the sites are hot. The second gives an array
//! its own method only after it has been used hot, by assignment, by
//! `defineProperty`, by `Object.assign`, by `Reflect` and by a computed key.
//!
//! The expected lines are Node 24's.

const SITES: &str = r#"
function mk(i) { return { v: i }; }
function pushLit(n) { const s = []; for (let i = 0; i < n; i++) s.push({ node: i, parent: s.length }); return s.length + ":" + s[n - 1].node + ":" + s[n - 1].parent; }
function incl(list, xs, n) { let c = 0; for (let i = 0; i < n; i++) if (list.includes(xs[i % xs.length])) c++; return c; }
function idx(list, o, n) { let c = 0; for (let i = 0; i < n; i++) c += list.indexOf(o.keys[i & 3]) + list.lastIndexOf(o.keys[i & 1]); return c; }
function mapf(list, fs, n) { let c = 0; for (let i = 0; i < n; i++) c += list.map(fs[i & 1]).length + list.filter(fs[2 + (i & 1)]).length + (list.some(fs[2]) ? 1 : 0) + (list.every(fs[3]) ? 1 : 0) + list.reduce(fs[4], mk(i).v & 1); return c; }
function joins(list, seps, n) { let s = 0; for (let i = 0; i < n; i++) s += list.join(seps[i & 1]).length + list.slice(seps.length - 1).length + list.concat(mk(i)).length + list.at(mk(i).v % 3); return s; }
function ends(n) { const a = []; let s = 0; for (let i = 0; i < n; i++) { a.push(mk(i).v); a.unshift(mk(i).v + 1); s += a.pop() + a.shift(); a.push(mk(i).v & 7); } a.sort((x, y) => x - y); a.reverse(); return s + ":" + a.length + ":" + a[0] + ":" + a.find((x) => x < 3) + ":" + a.findIndex((x) => x === 0); }
// The same sites over receivers of other kinds.
function mixed(recvs, n) { let out = ""; for (let i = 0; i < n; i++) { const r = recvs[i % recvs.length]; const a = r.includes(mk(i).v % 4); const b = r.indexOf(mk(i).v % 3); const c = typeof r.push; const d = r.slice(mk(1).v, 3); if (i < 12) out += a + "/" + b + "/" + c + "/" + String(d) + ";"; } return out; }
function callPush(r, n) { let last; for (let i = 0; i < n; i++) last = r.push(mk(i).v); return String(last); }
function readOnly(r, n) { let f; for (let i = 0; i < n; i++) f = r.push; return typeof f + ":" + (f === Array.prototype.push); }
const N = 5000;
const list = [3, 1, 4, 1, 5, 9, 2, 6];
const fs = [(x) => x * 2, (x) => x + 1, (x) => x > 4, (x) => x > 0, (a, x) => a + x];
console.log(pushLit(N), incl(list, [1, 7, 9, 0], N), idx(list, { keys: [1, 9, 8, 3] }, N));
console.log(mapf(list, fs, N), joins(list, [",", "--"], N), ends(N));
class Sub extends Array { push(x) { return "sub" + x; } includes(x) { return "subinc"; } indexOf() { return -7; } }
const own = [1, 2, 3]; own.push = function (x) { return "own" + x; }; own.includes = () => "owninc";
const named = [1, 2, 3]; named.label = "named";
const holey = [1, , 3]; const frozen = Object.freeze([1, 2, 3]); const empty = [];
const reproto = [1, 2, 3]; Object.setPrototypeOf(reproto, { push(x) { return "proto" + x; }, includes() { return "protoinc"; }, indexOf() { return -9; }, slice() { return "protoslice"; }, __proto__: Array.prototype });
const plain = { push: 1, includes(x) { return "plain" + x; }, indexOf(x) { return x + 100; }, slice(a, b) { return a + b; } };
const typed = new Uint8Array([1, 2, 3]);
function args() { return arguments; }
console.log(mixed([list, "a1b2c3", Sub.from([1, 2]), own, named, holey, frozen, empty, reproto, plain, typed, new Proxy([0, 1, 2], {})], N));
console.log(callPush(own, N), callPush(Sub.from([1]), N), callPush(reproto, N), callPush([], N), callPush(named, N), named.length);
console.log(readOnly(list, N), readOnly(own, N), readOnly(new Sub(), N), readOnly(args(1, 2), N), readOnly(typed, N), readOnly("str", N), readOnly(plain, N));
const nullProto = [1, 2]; Object.setPrototypeOf(nullProto, null);
try { callPush(nullProto, 10); } catch (e) { console.log(e instanceof TypeError, readOnly(nullProto, N)); }
// %Array.prototype% changes while the sites are hot.
const keptIncludes = Array.prototype.includes;
Array.prototype.includes = function () { return "replaced"; };
console.log(incl(list, [1, 7, 9, 0], N), mixed([list, own, "abc"], 6));
Array.prototype.includes = keptIncludes;
console.log(incl(list, [1, 7, 9, 0], N));
const keptLast = Array.prototype.lastIndexOf;
delete Array.prototype.lastIndexOf;
try { idx(list, { keys: [1, 9, 8, 3] }, N); } catch (e) { console.log("deleted:", e instanceof TypeError, typeof list.lastIndexOf); }
Object.defineProperty(Array.prototype, "lastIndexOf", { value: keptLast, writable: true, configurable: true, enumerable: false });
console.log(idx(list, { keys: [1, 9, 8, 3] }, N));
Object.defineProperty(Array.prototype, "at", { get() { return () => 1000; }, configurable: true });
console.log(joins(list, [",", "--"], N));
Array.prototype.extra = function () { return "extra"; };
function later(n) { const s = []; let t = 0; for (let i = 0; i < n; i++) { s.push(mk(i)); t += s.includes(mk(i)) ? 1 : 0; t += [i].map(fs[0])[0]; } return s.length + ":" + t + ":" + s.extra(); }
console.log(pushLit(N), later(N), mapf(list, fs, 500), callPush(own, 50), readOnly(list, N));
"#;

const LATE: &str = r#"
function mk(i) { return i & 3; }
function useIt(a, n) { let s = 0; for (let i = 0; i < n; i++) { s += a[mk(i) % a.length]; const r = a.includes(mk(i)); s += r === true ? 1 : r === false ? 0 : 100; const p = a.indexOf(mk(i)); s += typeof p === "number" ? p : 1000; } return s; }
function pushIt(a, n) { let last; for (let i = 0; i < n; i++) last = a.push(mk(i)); return String(last); }
function mapIt(a, n) { let s = 0; for (let i = 0; i < n; i++) { const m = a.map((x) => x + mk(i)); s += typeof m === "string" ? 7 : m.length; } return s; }
const N = 4000;
const a = [3, 1, 2, 0];
console.log(useIt(a, N), mapIt(a, N));
a.includes = function () { return "own"; };
console.log(useIt(a, N), typeof a.includes(1));
delete a.includes;
console.log(useIt(a, N));
Object.defineProperty(a, "indexOf", { get() { return () => "getter"; }, configurable: true });
console.log(useIt(a, N));
delete a.indexOf;
a.map = () => "mapped";
console.log(useIt(a, N), mapIt(a, N));
const b = [1, 2, 3];
console.log(pushIt(b, N), b.length);
b.push = function (x) { return "late" + x; };
console.log(pushIt(b, N), b.length);
const c = [5, 6, 7];
console.log(useIt(c, N));
Object.setPrototypeOf(c, { includes() { return "p"; }, indexOf() { return "q"; }, __proto__: Array.prototype });
console.log(useIt(c, N));
Object.setPrototypeOf(c, Array.prototype);
console.log(useIt(c, N));
const d = [1, 2, 3];
console.log(useIt(d, N));
d[Symbol.iterator] = function* () { yield 42; };
d.extra = 1;
console.log([...d].join(","), useIt(d, N), d.includes(2));
const e = [1, 2, 3]; console.log(useIt(e, N)); Object.freeze(e); console.log(useIt(e, N)); try { pushIt(e, 5); } catch (x) { console.log(x instanceof TypeError); }
const f = [1, 2, 3]; console.log(useIt(f, N)); f.length = 1; f[5] = 9; console.log(useIt(f, N), f.includes(undefined), f.indexOf(9));
// Own methods given by other routes, after the array was used hot.
const g = [3, 1, 2, 0]; console.log(useIt(g, N));
Object.defineProperty(g, "includes", { value: () => "defined", writable: true, configurable: true });
console.log(useIt(g, N));
const h = [3, 1, 2, 0]; console.log(useIt(h, N));
Object.assign(h, { indexOf: () => "assigned" });
console.log(useIt(h, N));
const k = [3, 1, 2, 0]; console.log(useIt(k, N), pushIt(k, 8));
Reflect.set(k, "includes", () => "reflected"); Reflect.defineProperty(k, "push", { value: (x) => "rpush" + x });
console.log(useIt(k, N), pushIt(k, 8), k.length);
const m = [3, 1, 2, 0]; console.log(useIt(m, N));
m["inc" + "ludes"] = () => "computed"; const key = "indexOf"; m[key] = () => "computed2";
console.log(useIt(m, N));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn array_method_read_sites_answer_as_node() {
    assert_eq!(
        run_ok(SITES),
        [
            "5000:4999:4999 2500 26250",
            "235000 185832 25000000:5000:7:2:4375",
            "false/-1/function/1,4;true/1/undefined/1b;subinc/-7/function/2;owninc/-1/function/2,3;false/0/function/2,3;true/-1/function/,3;true/-1/function/2,3;false/-1/function/;protoinc/-9/function/protoslice;plain1/100/number/4;true/0/undefined/2,3;false/2/function/1,2;",
            "own4999 sub4999 proto4999 5000 5003 5003",
            "function:true function:false function:false undefined:false undefined:false undefined:false number:false",
            "true undefined:false",
            "5000 replaced/-1/function/1,4;owninc/0/function/2,3;false/-1/undefined/bc;replaced/-1/function/1,4;owninc/0/function/2,3;false/-1/undefined/bc;",
            "2500",
            "deleted: true undefined",
            "26250",
            "5172500",
            "5000:4999:4999 5000:24995000:extra 23500 own49 function:true",
        ]
    );
}

#[test]
fn array_method_read_late_own_methods_answer_as_node() {
    assert_eq!(
        run_ok(LATE),
        [
            "16000 16000",
            "412000 string",
            "16000",
            "4010000",
            "16000 28000",
            "4003 4003",
            "late3 4003",
            "19000",
            "4423000",
            "19000",
            "12000",
            "42 12000 true",
            "12000",
            "12000",
            "true",
            "12000",
            "NaN true 5",
            "16000",
            "412000",
            "16000",
            "4010000",
            "16000 12",
            "412000 rpush3 12",
            "16000",
            "4406000",
        ]
    );
}

/// The same answers with the JIT compiling at once, with a collection at
/// every safe point, and with no native code.
#[test]
fn array_method_read_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("array_method_read_")
            .arg("--skip")
            .arg("all_modes")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("2 passed"),
            "both programs must run in {name} mode:\n{stdout}"
        );
    }
}
