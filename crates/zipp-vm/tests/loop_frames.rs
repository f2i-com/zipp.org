//! The loop runner's calls (`vm/loop_tier.rs`) against Node. A hot loop's call
//! of a function that is not a leaf pushes the callee's frame in the runner,
//! which goes on in it and returns from it; whatever the handlers do not do
//! there leaves the runner in that frame, and the dispatch loop takes over
//! from the top frame as after any call. Plain calls of a function with a
//! loop of its own; recursion from a loop; a class's methods with `this`
//! their receiver; fewer and more arguments than parameters, a return with no
//! value, a discarded result; callees that leave part-way (a string's
//! concatenation, an object literal, a `try`); a throw from a frame the
//! runner pushed, caught in the loop's function and above it; recursion past
//! the frame limit; closures, an arrow's captured `this`, a sloppy function's
//! and a strict one's; `for…of` over arrays with a call in the body, over a
//! hole and over an array the loop grows; generators, async functions and
//! constructors, which are not the runner's frames; a callee replaced while
//! the loop runs and one that is not a function; mutual recursion with loops
//! at each level; results that are not numbers; a collection while frames
//! the runner pushed are live. In the interpreter with the runner entering
//! as it does by itself, from every loop's first back-edge
//! (`ZIPP_LOOP_RUNNER_EAGER`) with and without the lite loop and without
//! frames of its own (`ZIPP_NO_LOOP_FRAMES`), and switched off; by default;
//! and with every function compiled at once.

const SOURCE: &str = r##"// Calls a hot loop makes of functions that are not leaves: the loop runner pushes their frames and goes on in
// them, returns from them, and leaves from inside one when an instruction is not its own.
var out = [];

// 1. A plain call of a function with a loop of its own.
function sumTo(n) { var s = 0; for (var i = 0; i < n; i++) { s = (s + i) | 0; } return s; }
function plain(n) { var t = 0; for (var i = 0; i < n; i++) { t = (t + sumTo(i & 15)) | 0; } return t; }
out.push(plain(5000));

// 2. Recursion from a loop, two calls a level.
function fib(n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); }
function fibs(n) { var t = 0; for (var i = 0; i < n; i++) { t += fib(i % 12); } return t; }
out.push(fibs(600));

// 3. Methods of a class: a frame for one that is not a leaf, `this` its receiver.
class Acc {
  constructor() { this.v = 0; this.n = 0; }
  add(x) { if (x > 3) { this.v = (this.v + x) | 0; } else { this.v = (this.v - 1) | 0; } this.n++; return this; }
  get total() { return this.v; }
}
function methods(n) { var a = new Acc(); for (var i = 0; i < n; i++) { a.add(i & 7); } return a.v + ":" + a.n; }
out.push(methods(4000));

// 4. Fewer and more arguments than parameters; a return with no value; a discarded result.
function three(a, b, c) { if (c === undefined) { if (b === undefined) return a; return a + b; } return a + b + c; }
function nothing(a) { if (a > 1e9) return 1; }
function args(n) {
  var t = 0;
  for (var i = 0; i < n; i++) { t += three(i); t += three(i, 1); t += three(i, 1, 2); t += three(i, 1, 2, 3); nothing(i); if (nothing(i) !== undefined) t = -1; }
  return t;
}
out.push(args(3000));

// 5. A callee that leaves the runner part-way (a string's concatenation, an object literal, a `try`).
function concat(i) { var s = "a"; if ((i & 3) === 0) s = s + i; return s.length; }
function literal(i) { var o = { x: i, y: i + 1 }; return o.x + o.y; }
function tries(i) { try { if (i % 97 === 0) throw new Error("x" + i); return i; } catch (e) { return -e.message.length; } }
function leaves(n) { var t = 0; for (var i = 0; i < n; i++) { t += concat(i) + literal(i) + tries(i); } return t; }
out.push(leaves(3000));

// 6. A throw from a frame the runner pushed, caught in the loop's function and above it.
function thrower(i) { if (i === 2500) { throw new RangeError("at " + i); } return i & 1; }
function catches(n) { var t = 0; try { for (var i = 0; i < n; i++) { t += thrower(i); } } catch (e) { return e.message + " after " + t; } return t; }
out.push(catches(4000));
function inner(n) { var t = 0; for (var i = 0; i < n; i++) { t += thrower(i); } return t; }
try { inner(4000); out.push("no throw"); } catch (e) { out.push(e instanceof RangeError ? "caught " + e.message : "wrong"); }

// 7. Recursion past the frame limit from a hot loop.
function deep(n) { return n === 0 ? 0 : 1 + deep(n - 1); }
function overflow() {
  var t = 0;
  for (var i = 0; i < 300; i++) { t += deep(i & 31); }
  try { for (var j = 0; j < 40; j++) { t += deep(j < 39 ? 5 : 1e7); } } catch (e) { return (e instanceof RangeError) + ":" + t; }
  return "none";
}
out.push(overflow());

// 8. Closures, an arrow's captured `this`, a sloppy function's `this`, a strict one's.
function counter() { var c = 0; return function (d) { if (d > 0) { c += d; } else { c -= 1; } return c; }; }
function closures(n) { var f = counter(), g = counter(), t = 0; for (var i = 0; i < n; i++) { t = f(i & 3) + g(1); } return t; }
out.push(closures(3000));
var holder = {
  k: 7,
  run: function (n) { var f = (x) => { if (x < 0) return 0; return this.k + x; }; var t = 0; for (var i = 0; i < n; i++) { t += f(i & 1); } return t; }
};
out.push(holder.run(3000));
function sloppyThis(x) { if (x < 0) return 0; return this === globalThis ? 1 : 2; }
function strictThis(x) { "use strict"; if (x < 0) return 0; return this === undefined ? 10 : 20; }
function thises(n) { var t = 0; for (var i = 0; i < n; i++) { t += sloppyThis(i) + strictThis(i); } return t; }
out.push(thises(2000));

// 9. `for…of` over arrays with a call in the body; a hole and a grown array.
function weigh(x) { if (x === undefined) return 100; return x * 2; }
function forOf(n) {
  var a = [], t = 0;
  for (var i = 0; i < n; i++) a.push(i & 15);
  for (var r = 0; r < 20; r++) { for (var v of a) { t += weigh(v); } }
  var holey = [1, , 3]; for (var h of holey) { t += weigh(h); }
  var grow = [1, 2, 3]; for (var g of grow) { if (grow.length < 8) grow.push(g + 10); t += g; }
  return t;
}
out.push(forOf(500));

// 10. Generators, async functions and constructors called from a hot loop are not the runner's frames.
function* gen(i) { yield i; yield i + 1; }
function ctor(i) { this.i = i; }
async function later(i) { return i; }
function others(n) {
  var t = 0, p = 0;
  for (var i = 0; i < n; i++) { var g = gen(i); t += g.next().value + g.next().value; t += new ctor(i).i; later(i).then(function (v) { p += v; }); }
  return t;
}
out.push(others(1500));

// 11. A callee replaced while the loop runs; a callee that is not a function.
var swap = function (i) { if (i < 0) return 0; return 1; };
function swaps(n) {
  var t = 0;
  for (var i = 0; i < n; i++) { t += swap(i); if (i === 1000) swap = function (i) { if (i < 0) return 0; return 3; }; }
  try { for (var j = 0; j < 100; j++) { if (j === 50) swap = 5; t += swap(j); } } catch (e) { return (e instanceof TypeError) + ":" + t; }
  return t;
}
out.push(swaps(2000));

// 12. Mutual recursion with loops at each level, and results that are not numbers.
function evenLen(a, d) { var t = 0; for (var i = 0; i < a.length; i++) { t += d > 0 ? oddLen(a, d - 1) : a[i]; } return t; }
function oddLen(a, d) { var t = 0; for (var i = 0; i < a.length; i++) { t += d > 0 ? evenLen(a, d - 1) : -a[i]; } return t; }
out.push(evenLen([1, 2, 3, 4], 5));
function pick(i) { if (i % 3 === 0) return "s"; if (i % 3 === 1) return null; return { z: i }; }
function mixed(n) { var s = 0, k = 0, o = 0; for (var i = 0; i < n; i++) { var r = pick(i); if (r === "s") s++; else if (r === null) k++; else o += r.z; } return s + ":" + k + ":" + o; }
out.push(mixed(3000));

// 13. A collection while frames the runner pushed are live.
function alloc(i) { var a = [i, i + 1, i + 2]; return a.length + a[1]; }
function churn(n) { var t = 0; for (var i = 0; i < n; i++) { t = (t + alloc(i)) | 0; } return t; }
out.push(churn(200000));

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "174776\n11600\n9000:4000\n18015000\n13458976\nat 2500 after 1250\ncaught at 2500\ntrue:4725\n6750\n22500\n22000\n149233\n3374250\ntrue:4148\n-10240\n1000:1000:1500500\n-1474136480";

/// Every switch a mode below may set, cleared before each run.
const SWITCHES: [&str; 6] = [
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_LOOP_RUNNER_EAGER",
    "ZIPP_NO_LOOP_RUNNER",
    "ZIPP_NO_LOOP_FRAMES",
    "ZIPP_NO_LITE",
];

#[test]
fn loop_frames_worker() {
    if std::env::var_os("ZIPP_LOOP_FRAMES_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn loop_frames_match_node() {
    if std::env::var_os("ZIPP_LOOP_FRAMES_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let modes: [(&str, &[&str]); 7] = [
        ("default", &[]),
        ("interpreter", &["ZIPP_NOJIT"]),
        ("interpreter, runner from the first back-edge", &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER"]),
        (
            "interpreter, runner from the first back-edge, no lite loop",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LITE"],
        ),
        (
            "interpreter, runner from the first back-edge, no frame of its own",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LOOP_FRAMES"],
        ),
        ("interpreter, no runner", &["ZIPP_NOJIT", "ZIPP_NO_LOOP_RUNNER"]),
        ("compile-at-once", &["ZIPP_JIT_THRESHOLD"]),
    ];
    for (mode, vars) in modes {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["loop_frames_worker", "--exact", "--nocapture"])
            .env("ZIPP_LOOP_FRAMES_WORKER", "1");
        for v in SWITCHES {
            command.env_remove(v);
        }
        for v in vars {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
