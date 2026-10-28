//! for-of over arrays from compiled code, whose dense-Array steps are emitted
//! through the array mirror (`emit_arr_mirror_iter_step`): plain arrays,
//! holes read through the prototype chain, arrays that grow, shrink, are
//! truncated and spliced while iterated, sparse arrays, `arguments`, an Array
//! subclass, named properties, a string's code points, Sets, Maps and a
//! TypedArray, an allocating body, break / continue / return, nested loops,
//! and iteration replaced on Array.prototype and on %ArrayIteratorPrototype%
//! after the loops compiled. Run interpreted, at the default thresholds, with
//! every function compiled at once, under GC stress and with the emitted step
//! off; the expected line is Node 24's.

const EXPECTED: &str = "200000|1;undefined;3;|1;P;3;|0;P;2;undefined;4;|8:1.2.11.12.21.22.31.32|123|123|145|1;undefined;undefined;undefined;undefined;6;|6|18|6|104.233.128512.111|3;4;|1,2;|6|12000|v4|3|6|100|300|100|10";

#[test]
fn for_of_array_steps_child() {
    if std::env::var_os("ZIPP_FOR_OF_STEPS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn for_of_steps_match_node() {
    if std::env::var_os("ZIPP_FOR_OF_STEPS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("step-off", Some(("ZIPP_NO_ITER_MIRROR_STEP", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "for_of_array_steps_child", "--nocapture"])
            .env("ZIPP_FOR_OF_STEPS_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// for-of over arrays from compiled code: plain arrays, holes, arrays that
// grow, shrink and are spliced while iterated, sparse arrays, arguments,
// a subclass, named properties, an allocating body, break / continue /
// return, nested loops, and iteration replaced on the prototypes.
const out = [];
function sum(a) { let s = 0; for (const x of a) s += x; return s; }
const a4 = [1, 2, 3, 4];
let t = 0; for (let r = 0; r < 20000; r++) t += sum(a4);
out.push(t);
// Holes read through the prototype chain; a hole filled by Array.prototype.
function show(a) { let s = ""; for (const x of a) s += String(x) + ";"; return s; }
const holey = [1, , 3];
for (let r = 0; r < 2000; r++) show(holey);
out.push(show(holey));
Array.prototype[1] = "P"; out.push(show(holey), show([0, , 2, , 4])); delete Array.prototype[1];
// Growth, shrink, length and splice during the loop.
function grow(a) { let n = 0; for (const x of a) { n++; if (a.length < 8) a.push(x + 10); } return n + ":" + a.join("."); }
function shrink(a) { let s = ""; for (const x of a) { s += x; a.pop(); } return s; }
function trunc(a) { let s = ""; for (const x of a) { s += x; if (x === 2) a.length = 3; } return s; }
function spl(a) { let s = ""; for (const x of a) { s += x; if (x === 1) a.splice(0, 2); } return s; }
for (let r = 0; r < 2000; r++) { grow([1, 2]); shrink([1, 2, 3, 4]); trunc([1, 2, 3, 4, 5]); spl([1, 2, 3, 4, 5]); }
out.push(grow([1, 2]), shrink([1, 2, 3, 4, 5, 6]), trunc([1, 2, 3, 4, 5]), spl([1, 2, 3, 4, 5]));
// Sparse, arguments, a subclass, named properties, other iterables.
const sparse = []; sparse[0] = 1; sparse[5] = 6;
function args() { let s = 0; for (const x of arguments) s += x; return s; }
class MyArr extends Array {}
const sub = MyArr.from([5, 6, 7]);
const named = [1, 2, 3]; named.tag = "x";
for (let r = 0; r < 2000; r++) { show(sparse); args(1, 2, 3); sum(sub); sum(named); }
out.push(show(sparse), args(1, 2, 3), sum(sub), sum(named), (() => { const s = []; for (const ch of "h\u00e9\ud83d\ude00o") s.push(ch.codePointAt(0)); return s.join("."); })(), show(new Set([3, 4])), show(new Map([[1, 2]])), sum(new Int8Array([1, 2, 3])));
// An allocating body (collections while iterating) and early exits.
function build(a) { const r = []; for (const x of a) r.push({ x, s: "v" + x }); return r; }
let objs = 0; for (let r = 0; r < 3000; r++) objs += build(a4).length;
out.push(objs, build(a4)[3].s);
function firstBig(a) { for (const x of a) { if (x > 2) return x; } return -1; }
function skipOdd(a) { let s = 0; for (const x of a) { if (x & 1) continue; s += x; if (x > 3) break; } return s; }
function nested(a) { let s = 0; for (const x of a) for (const y of a) s += x * y; return s; }
for (let r = 0; r < 3000; r++) { firstBig(a4); skipOdd([1, 2, 3, 4, 5, 6]); nested(a4); }
out.push(firstBig(a4), skipOdd([1, 2, 3, 4, 5, 6]), nested(a4));
// The iteration protocol replaced, after the loops compiled.
const saved = Array.prototype[Symbol.iterator];
Array.prototype[Symbol.iterator] = function* () { yield 100; yield 200; };
out.push(sum(a4));
Array.prototype[Symbol.iterator] = saved;
const AIP = Object.getPrototypeOf([][Symbol.iterator]());
const savedNext = AIP.next;
AIP.next = function () { const r = savedNext.call(this); if (!r.done) r.value *= 10; return r; };
out.push(sum(a4));
AIP.next = savedNext;
out.push(sum(a4));
console.log(out.join("|"));
"####;
