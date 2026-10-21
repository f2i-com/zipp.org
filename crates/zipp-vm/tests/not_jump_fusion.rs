//! `!x` and the conditional jump on it run as one superinstruction in the
//! interpreter's fused copy (`Instr::FuseNotJump`), against Node.
//!
//! The probe takes `if (!x)`, `if (!x) … else …`, a `!x` kept in a variable
//! the branch then reads again, `while (!…)`, `do … while (!…)`, a `for` test,
//! a conditional and `&&`/`||` of negations over every kind of value: zeros,
//! NaN, strings, null and undefined, booleans, BigInts, objects, arrays,
//! functions, a Symbol and a Boolean wrapper. A hot loop runs the JIT tiers
//! too. The expected lines are Node's (v24).

const PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
const values = [0, -0, 1, -1, NaN, Infinity, "", "0", "a", null, undefined, true, false, 0n, 1n, {}, [], () => 0, Symbol("s"), new Boolean(false), 2 ** 40, -(2 ** 40)];
function ifNot(v) { if (!v) return "F"; return "T"; }
function ifNotElse(v) { var r; if (!v) { r = 1; } else { r = 2; } return r; }
function keepNot(v) { var z = !v; if (z) return "z:" + z; return "nz:" + z; }
function whileNot(xs) { var i = 0; while (!xs[i]) i++; return i; }
function doNot(xs) { var i = 0; do { i++; } while (!xs[i]); return i; }
function forNot(n) { var c = 0; for (var i = 0; !(i >= n); i++) c += i; return c; }
function ternaryNot(v) { return !v ? "a" : "b"; }
function andNot(a, b) { if (!a && !b) return "neither"; if (!a || !b) return "one"; return "both"; }
log("ifNot", values.map(ifNot).join(""));
log("ifNotElse", values.map(ifNotElse).join(""));
log("keepNot", values.map(keepNot).join(","));
log("ternaryNot", values.map(ternaryNot).join(""));
log("whileNot", whileNot([0, "", null, 5]), whileNot([1]), doNot([0, 0, 0, 7]));
log("forNot", forNot(5), forNot(0));
log("andNot", andNot(0, 0), andNot(1, 0), andNot(1, 1), andNot(null, "x"));
let acc = 0;
for (let k = 0; k < 200000; k++) {
  acc += ifNot(k & 3) === "F" ? 1 : 0;
  acc += keepNot(k % 5).length;
  acc += whileNot([0, k & 1, 1]);
}
log("hot", acc);
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"ifNot FFTTFTFTTFFTFFTTTTTTTT
ifNotElse 1122121221121122222222
keepNot z:true,z:true,nz:false,nz:false,z:true,nz:false,z:true,nz:false,nz:false,z:true,z:true,nz:false,z:true,z:true,nz:false,nz:false,nz:false,nz:false,nz:false,nz:false,nz:false,nz:false
ternaryNot aabbababbaabaabbbbbbbb
whileNot 3 0 3
forNot 10 0
andNot neither one both one
hot 1870000"###;

fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = zipp_vm::run(&src).expect("the probe compiles");
            assert!(out.error.is_none(), "runtime error: {:?}", out.error);
            out.output.join("\n")
        })
        .expect("spawn the probe thread")
        .join()
        .expect("the probe thread finishes")
}

#[test]
fn negated_branches_match_node() {
    assert_eq!(run(PROBE), EXPECTED);
}
