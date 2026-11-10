//! The loop runner's string-building handlers (`vm/loop_tier.rs`:
//! `StrConcatChain`, `StrAppendInPlace`, `ToStr`) against Node, and a loop
//! that meets an instruction on some iterations only. `s += x` over numbers,
//! strings, booleans, `null`, `undefined`, `-0`, `1e21` and `NaN`; a chain of
//! five links whose accumulator is fresh each iteration; template literals
//! with the string hint (`toString` before `valueOf`) and a nested template;
//! an operand whose conversion runs a loop (the runner entered again under
//! the handler), in all three forms; a conversion that throws, caught in the
//! loop, around it and in a caller, and a Symbol's TypeError; strings built
//! in functions the loop calls, one through a closure's accumulator; a
//! request loop where every fourth iteration takes another path; a conversion
//! that puts an accessor behind a global the loop counts in, and one that
//! recurses deep enough to move the register file; growth to a large string,
//! and a rope flattened under the loop. In the interpreter with the runner
//! entering as it does by itself, from every loop's first back-edge
//! (`ZIPP_LOOP_RUNNER_EAGER`) with and without the lite loop and without
//! frames of its own (`ZIPP_NO_LOOP_FRAMES`), and switched off; by default;
//! and with every function compiled at once.

const SOURCE: &str = r##"// String building in hot loops: `s += x`, chains `a + b + c`, template literals; operands that are numbers,
// strings, objects whose conversion runs code (loops, throws, recurses, changes a global), Symbols; in the loop's
// own frame and in functions it calls; and a request loop that builds a string on some iterations only.
var out = [];
function hash(s) { var h = 0; for (var i = 0; i < s.length; i++) h = (Math.imul(h, 31) + s.charCodeAt(i)) | 0; return h; }

// 1. `s += x` with numbers, strings, booleans, null and undefined.
(function () {
  var s = "", parts = [1, "a", true, null, undefined, 2.5, -0, "é", 1e21, NaN];
  for (var i = 0; i < 3000; i++) { s += parts[i % parts.length]; if ((i & 255) === 255) s += "|"; }
  out.push("append " + s.length + " " + hash(s));
})();

// 2. A chain of three and more, its accumulator fresh each iteration.
(function () {
  var t = 0;
  for (var i = 0; i < 3000; i++) { var s = "k" + i + ":" + (i & 7) + "/" + (i * 0.5) + "!"; t = (t + hash(s)) | 0; }
  out.push("chain " + t);
})();

// 3. Template literals: the string hint (toString before valueOf), numbers, nested templates.
(function () {
  var calls = [];
  var both = { toString() { calls.push("s"); return "S"; }, valueOf() { calls.push("v"); return "V"; } };
  var t = 0, last = "";
  for (var i = 0; i < 3000; i++) { last = `${i}-${i & 1 ? both : "x"}-${`in${i % 3}`}`; t = (t + hash(last)) | 0; }
  out.push("template " + t + " " + last + " " + calls.length + calls[0] + " plus:" + ("" + both) + calls[calls.length - 1]);
})();

// 4. An object whose conversion loops (the runner entered again under the handler), in all three forms.
(function () {
  var n = 0;
  var busy = { toString() { var t = 0; for (var i = 0; i < 40; i++) t += i; n++; return "b" + t; } };
  var s = "", t = 0;
  for (var i = 0; i < 2500; i++) {
    if ((i & 63) === 0) s += busy;
    var c = "<" + i + ">" + ((i & 127) === 0 ? busy : i);
    var d = `${(i & 255) === 0 ? busy : 1}`;
    t = (t + c.length + d.length) | 0;
  }
  out.push("busy " + n + " " + s.length + " " + t);
})();

// 5. A conversion that throws, caught in the loop, around it, and in a caller; a Symbol's TypeError.
(function () {
  var bad = { toString() { throw new RangeError("no"); } };
  var caught = 0, sym = Symbol("q");
  for (var i = 0; i < 3000; i++) {
    try { var s = "v" + i + ((i % 1000) === 999 ? bad : "") + "."; } catch (e) { caught += e instanceof RangeError ? 1 : 100; }
    try { var u = `${(i % 1000) === 500 ? sym : i}`; } catch (e) { caught += e instanceof TypeError ? 10 : 1000; }
  }
  var around = "none";
  try { var acc = ""; for (var j = 0; j < 3000; j++) { acc += j === 2222 ? sym : "x"; } } catch (e) { around = e.constructor.name + j; }
  function builds(k) { var s = ""; for (var m = 0; m < 2000; m++) { s += m === k ? bad : m; } return s.length; }
  var caller = 0;
  try { caller = builds(-1); caller += builds(1500); } catch (e) { caller = e.message + caller; }
  out.push("throws " + caught + " " + around + " " + caller);
})();

// 6. String building in functions the loop calls (frames), with closures over the accumulator.
(function () {
  function label(a, b) { return "id" + a + "-" + b + "#"; }
  function tpl(o) { return `${o.a}:${o.b}:${o.a + o.b}`; }
  var log = "";
  function note(x) { log += x; if (log.length > 4000) log = ""; return log.length; }
  var t = 0;
  for (var i = 0; i < 4000; i++) { t = (t + hash(label(i, i & 3)) + tpl({ a: i, b: "z" }).length + note(i & 15)) | 0; }
  out.push("frames " + t + " " + log.length);
})();

// 7. A request loop: every fourth iteration takes a path that builds a string.
(function () {
  var routes = new Map(), hits = 0;
  routes.set("/a", function (r) { return { status: 200, body: "a:" + r.n + ":" + hits }; });
  routes.set("/b", function (r) { return { status: 201, body: r.q }; });
  routes.set("/c", function (r) { return { status: r.n & 1 ? 202 : 400, body: `c${r.n & 7}` }; });
  function make(i) { var k = i % 4; return k === 0 ? { path: "/a", n: i } : k === 1 ? { path: "/b", q: "q" + (i & 31) } : k === 2 ? { path: "/c", n: i } : { path: "/d" }; }
  function handle(r) { hits++; var h = routes.get(r.path); return h === undefined ? { status: 404, body: "missing" } : h(r); }
  var sum = 0;
  for (var i = 0; i < 20000; i++) { var res = handle(make(i)); sum = (Math.imul(sum ^ res.status, 33) + res.body.length) | 0; }
  out.push("requests " + sum + " " + hits);
})();

// 8. A conversion that changes how a global is reached, and one that recurses deep enough to move the registers.
counterG = 0;
(function () {
  var flip = { toString() { Object.defineProperty(globalThis, "counterG", { get() { return 1000; }, set(v) {}, configurable: true }); return "f"; } };
  var s = "", seen = 0;
  for (var i = 0; i < 3000; i++) { counterG = counterG + 1; s += i === 1500 ? flip : ""; seen = counterG; }
  function deep(n) { return n === 0 ? 0 : 1 + deep(n - 1); }
  var grow = { toString() { return "g" + deep(3000); } };
  var t = "";
  for (var j = 0; j < 2000; j++) { var a = j, b = j * 2, c = "c" + j; t = "p" + (j === 1000 ? grow : a) + b + c; }
  out.push("globals " + seen + " " + s + " " + t);
})();

// 9. Growth to a large string and a rope's flattening under the loop.
(function () {
  var s = "";
  for (var i = 0; i < 20000; i++) { s += "abcdefgh"; }
  var r = "";
  for (var j = 0; j < 300; j++) { r = r + s.slice(j, j + 40) + j; if ((j & 63) === 63) r.charCodeAt(5); }
  out.push("large " + s.length + " " + hash(s.slice(-64)) + " " + r.length + " " + hash(r));
})();

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "append 9611 -2015608104\nchain 511555524\ntemplate -727525752 2999-S-in2 1500s plus:Vv\nbusy 70 160 25320\nthrows 33 TypeError2222 no6890\nframes -834845146 1498\nrequests -1899231283 20000\nglobals 1000 f p19993998c1999\nlarge 160000 -320121824 12790 -1766947548";

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
fn loop_strings_worker() {
    if std::env::var_os("ZIPP_LOOP_STRINGS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn loop_strings_match_node() {
    if std::env::var_os("ZIPP_LOOP_STRINGS_WORKER").is_some() {
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
            .args(["loop_strings_worker", "--exact", "--nocapture"])
            .env("ZIPP_LOOP_STRINGS_WORKER", "1");
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
