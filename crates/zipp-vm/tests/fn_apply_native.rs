//! Native `fn.apply(thisArg, argArray)` fast path.
//!
//! `fn.apply(thisArg, arr)` previously missed the native call stub and paid two
//! runtime helper round trips per call. Compiled code now checks `%Function.prototype%.apply`
//! is pristine, `fn` has no own `apply` or prototype override, and `argArray` is
//! nullish or a settled plain dense Array of length <= 8 with no holes, then enters
//! `cross3_dyn_stub` directly (both for fused `CallMethod` and split `GetProp` +
//! `CallWithThis`). Rest-parameter arrays are allocated with `arr_mirror` settled so
//! forwarding wrappers (`function any(...parameters) { checks[i].apply(this, parameters) }`)
//! take the native lane on every iteration.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];
const log = (...a) => out.push(a.join(" "));
const N = 2000;

function addStrict(a, b, c) { "use strict"; return (this === undefined ? "U" : this === null ? "N" : typeof this === "object" ? this.tag : String(this)) + ":" + a + "," + b + "," + c; }
function addSloppy(a, b, c) { return (this === globalThis ? "G" : typeof this === "object" ? (this.tag ?? typeof this.valueOf()) : typeof this) + ":" + a + "," + b + "," + c; }
const arrowThis = { tag: "LEX" };
const makeArrow = function () { return (a, b) => this.tag + ":" + a + "," + b; };
const arrowFn = makeArrow.call(arrowThis);

function sum8(a, b, c, d, e, f, g, h) { "use strict"; return (a || 0) + (b || 0) + (c || 0) + (d || 0) + (e || 0) + (f || 0) + (g || 0) + (h || 0); }
function sum9(a, b, c, d, e, f, g, h, i) { "use strict"; return (a || 0) + (b || 0) + (c || 0) + (d || 0) + (e || 0) + (f || 0) + (g || 0) + (h || 0) + (i || 0); }

function callApply(fn, t, arr, n) { let r; for (let i = 0; i < n; i++) r = fn.apply(t, arr); return r; }
function callApply0(fn, n) { let r; for (let i = 0; i < n; i++) r = fn.apply(); return r; }
function callApply1(fn, t, n) { let r; for (let i = 0; i < n; i++) r = fn.apply(t); return r; }
function callApply3(fn, t, arr, n) { let r; for (let i = 0; i < n; i++) r = fn.apply(t, arr, 999); return r; }
function callSplit(fn, t, arr, n) { let r; const pick = (x) => x; for (let i = 0; i < n; i++) r = fn.apply(pick(t), pick(arr)); return r; }

const box = { tag: "B" };
log("basic",
  callApply(addStrict, box, [1, 2, 3], N),
  callApply(addStrict, null, [4, 5], N),
  callApply(addStrict, undefined, [], N),
  callApply(addStrict, 7, [1], N),
  callApply(addSloppy, box, [1, 2, 3], N),
  callApply(addSloppy, null, [4, 5], N),
  callApply(addSloppy, undefined, [6], N),
  callApply(addSloppy, 7, [1], N),
  callApply(arrowFn, box, [8, 9], N),
  callApply0(addStrict, N),
  callApply1(addStrict, box, N),
  callApply(addStrict, box, null, N),
  callApply(addStrict, box, undefined, N),
  callApply3(addStrict, box, [10, 20, 30], N),
  callSplit(addStrict, box, [11, 22, 33], N)
);

log("lens",
  callApply(sum8, null, [], N),
  callApply(sum8, null, [1, 2, 3, 4, 5, 6, 7, 8], N),
  callApply(sum9, null, [1, 2, 3, 4, 5, 6, 7, 8, 9], N)
);

const holey = [1, , 3];
log("holey1", callApply(addStrict, box, holey, N));
Array.prototype[1] = 77;
log("holey2", callApply(addStrict, box, holey, N));
delete Array.prototype[1];
log("holey3", callApply(addStrict, box, holey, N));

function viaArgsSloppy(a, b, c) { a = 99; return callApply(addStrict, box, arguments, 100); }
function viaArgsStrict(a, b, c) { "use strict"; a = 99; return callApply(addStrict, box, arguments, 100); }
log("args", viaArgsSloppy(1, 2, 3), viaArgsStrict(1, 2, 3));

const frozen = Object.freeze([5, 6, 7]);
class SubArr extends Array {}
const sub = new SubArr(2, 4, 6);
log("frozen_sub", callApply(addStrict, box, frozen, N), callApply(addStrict, box, sub, N));

const mutArr = [1, 2, 3];
function mutator(a, b, c) {
  "use strict";
  if (mutArr.length < 10) mutArr.push(a + b + c);
  return a + ":" + b + ":" + c + ":" + mutArr.length;
}
log("mutate", callApply(mutator, null, mutArr, 20));

function bailer(a, b) {
  if (a === 42) return bailer.arguments.length + ":" + bailer.arguments[0] + ":" + bailer.arguments[1];
  return a + b;
}
let bailOut = "";
for (let i = 0; i < 400; i++) bailOut = callApply(bailer, null, [i === 399 ? 42 : 1, 2], 1);
log("bail", bailOut);

function thrower(a, b) { "use strict"; if (a < 0) throw new RangeError("neg:" + b); return a + b; }
let caught = "";
for (let i = 0; i < 400; i++) {
  try { callApply(thrower, null, [i === 399 ? -1 : 1, 5], 1); } catch (e) { caught = e.name + ":" + e.message; }
}
log("throw", caught);

function mkAny(types) {
  const checks = types.map((t) => function ok(node) { return node.type === t; });
  return function any(...parameters) {
    let idx = -1;
    while (++idx < checks.length) {
      if (checks[idx].apply(this, parameters)) return true;
    }
    return false;
  };
}
const anyCheck = mkAny(["a", "b", "c", "d", "e"]);
let hits = 0;
for (let i = 0; i < N; i++) {
  if (anyCheck({ type: i % 2 === 0 ? "e" : "z" }, i, null)) hits++;
}
log("rest_any", hits);

const objApply = { apply(t, a) { return "obj:" + t + ":" + a.length; } };
function ownApplyFn(x) { return x + 1; }
ownApplyFn.apply = function (t, a) { return "own:" + t + ":" + a[0]; };
function reprotoFn(x) { return x + 1; }
Object.setPrototypeOf(reprotoFn, { apply(t, a) { return "proto:" + a[0]; } });
log("overrides",
  callApply(addStrict, box, [1, 2, 3], 200),
  callApply(objApply, "T", [1, 2], 200),
  callApply(ownApplyFn, "T", [9], 200),
  callApply(reprotoFn, "T", [8], 200),
  callSplit(ownApplyFn, "T", [7], 200)
);

const savedApply = Function.prototype.apply;
Function.prototype.apply = function (t, a) { return "patched:" + (a ? a.length : -1); };
log("patched", callApply(addStrict, box, [1, 2], 200), callSplit(addStrict, box, [1, 2, 3], 200));
Function.prototype.apply = savedApply;
log("restored", callApply(addStrict, box, [1, 2, 3], 200), callSplit(addStrict, box, [4, 5, 6], 200));

log("call_apply", Function.prototype.call.apply(addStrict, [box, 7, 8, 9]));
log("apply_apply", Function.prototype.apply.apply(addStrict, [box, [7, 8, 9]]));
function recApply(n) { return n === 0 ? "done" : recApply.apply(null, [n - 1]); }
log("rec_apply", recApply(8));

console.log(out.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
        .iter()
        .flat_map(|l| l.lines().map(str::to_string))
        .collect()
}

#[test]
fn fn_apply_native_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "basic B:1,2,3 N:4,5,undefined U:undefined,undefined,undefined 7:1,undefined,undefined B:1,2,3 G:4,5,undefined G:6,undefined,undefined number:1,undefined,undefined LEX:8,9 U:undefined,undefined,undefined B:undefined,undefined,undefined B:undefined,undefined,undefined B:undefined,undefined,undefined B:10,20,30 B:11,22,33",
            "lens 0 36 45",
            "holey1 B:1,undefined,3",
            "holey2 B:1,77,3",
            "holey3 B:1,undefined,3",
            "args B:99,2,3 B:1,2,3",
            "frozen_sub B:5,6,7 B:2,4,6",
            "mutate 1:2:3:10",
            "bail 2:42:2",
            "throw RangeError:neg:5",
            "rest_any 1000",
            "overrides B:1,2,3 obj:T:2 own:T:9 proto:8 own:T:7",
            "patched patched:2 patched:3",
            "restored B:1,2,3 B:4,5,6",
            "call_apply B:7,8,9",
            "apply_apply B:7,8,9",
            "rec_apply done",
        ]
    );
}

#[test]
fn fn_apply_native_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("fn_apply_native_answers_as_node")
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
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
