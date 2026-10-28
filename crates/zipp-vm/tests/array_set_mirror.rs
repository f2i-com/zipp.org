//! `a[i] = v` overwriting a present element of a dense Array in compiled
//! code through its array mirror entry (`emit_arr_mirror_set`), beside every
//! store it must leave to the general path: young objects stored into an old
//! array and then collections (the store's nursery barrier: without it the
//! default mode loses all 64), frozen, sealed and non-writable elements,
//! holes, growth past the end, double and negative keys, an Array.prototype
//! index setter, a mapped `arguments` object, a TypedArray, a plain object,
//! and a sloppy store into a frozen array. Run interpreted, at the default
//! thresholds, with every function compiled at once, under GC stress, and
//! with the overwrite switched off; the expected line is Node 24's.

const EXPECTED: &str = "2997,2998,2999,64,TypeError,2,1,9,3,TypeError,2,0,5,2,true,6,1,2,3,,,6,7,8,4,9,2,1,33,,1/10,44,c,198,1";

#[test]
fn array_set_mirror_child() {
    if std::env::var_os("ZIPP_ARRAY_SET_MIRROR_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn array_stores_through_the_mirror_stay_exact() {
    if std::env::var_os("ZIPP_ARRAY_SET_MIRROR_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("overwrite-off", Some(("ZIPP_NO_ARR_SET_MIRROR", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "array_set_mirror_child", "--nocapture"])
            .env("ZIPP_ARRAY_SET_MIRROR_CHILD", "1")
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

const PROGRAM: &str = r####"// `a[i] = v` overwriting a present element in compiled code through the
// array mirror, beside every store it must leave to the general path.
"use strict";
const out = [];
function at(a, i) { return a[i]; }
function put(a, i, v) { at(a, 0); a[i] = v; return a[i]; }
function sloppyPut(a, i, v) { return (new Function("a", "i", "v", "a[i] = v; return a[i];"))(a, i, v); }
const warm = [1, 2, 3];
for (let r = 0; r < 3000; r++) put(warm, r % 3, r);
out.push(warm.join());
// Young objects stored into an old array, then collections: every value
// must survive (the store's nursery barrier).
const old = new Array(64).fill(0).map((_, i) => ({ i }));
for (let g = 0; g < 200000; g++) { const junk = { g }; if (junk.g === -1) out.push("x"); }
function fill(n) { for (let k = 0; k < n; k++) put(old, k & 63, { v: k, s: "s" + k }); }
fill(64 * 50);
for (let g = 0; g < 400000; g++) { const junk = [g, { g }]; if (junk[0] === -1) out.push("x"); }
let ok = 0; for (let k = 0; k < 64; k++) if (old[k].s === "s" + old[k].v && old[k].v % 64 === k) ok++;
out.push(ok);
// Stores the mirror must not take.
const frozen = Object.freeze([1, 2, 3]);
try { put(frozen, 1, 9); out.push("no throw"); } catch (e) { out.push(e.constructor.name); }
out.push(frozen[1]);
const sealed = Object.seal([1, 2, 3]); put(sealed, 1, 9); out.push(sealed.join());
const ro = [1, 2, 3]; Object.defineProperty(ro, 1, { value: 2, writable: false });
try { put(ro, 1, 9); out.push("no throw"); } catch (e) { out.push(e.constructor.name); }
out.push(ro[1]);
const holes = [0, , 2]; put(holes, 1, 5); out.push(holes.join(), 1 in holes);
const grow = [1, 2]; put(grow, 2, 3); put(grow, 5, 6); out.push(grow.length, grow.join());
out.push(put([1, 2, 3], 1.0, 7), put([1, 2, 3], -1, 8), [1, 2, 3, 8].length);
const neg = [1, 2]; put(neg, -1, 9); out.push(neg[-1], neg.length);
let setterHits = 0;
Object.defineProperty(Array.prototype, 7, { set(v) { setterHits++; }, configurable: true });
const sp = [0, 1, 2, 3, 4, 5, 6]; sp.length = 9; put(sp, 7, 1); put(sp, 3, 33);
delete Array.prototype[7];
out.push(setterHits, sp[3], sp[7]);
function argsStore(x, y) { put(arguments, 0, 10); return x + "/" + arguments[0]; }
out.push(argsStore(1, 2));
const typed = new Int8Array(3); put(typed, 1, 300); out.push(typed[1]);
const obj = { 0: "a", 1: "b" }; put(obj, 1, "c"); out.push(obj[1]);
const big = []; for (let k = 0; k < 100; k++) big.push(k); for (let k = 0; k < 100; k++) put(big, k, big[k] * 2); out.push(big[99]);
out.push(sloppyPut(Object.freeze([1]), 0, 2));
console.log(out.join(","));
"####;
