//! The loop runner's property and method handlers (`vm/loop_tier.rs`) at
//! sites that several object layouts reach, against Node: they ask the
//! site's ways as the dispatch arms do (`ic_get_prop_way0`,
//! `ic_set_prop_way0`, `ic_call_method_mirror`). One read, one accessor and
//! one method site over four classes whose field sits at different slots,
//! one of them calling `super`; a read site where the name is own data,
//! inherited data through one and three prototypes, a getter and absent, by
//! layout; a write site over three layouts' slots, a setter, a frozen
//! object, a prototype's setter and a read-only inherited property, and a
//! store that adds the property; forty layouts through one read, one write
//! and one call site, and dictionaries; layouts and prototypes changed under
//! the loop (a property added to an object and to its prototype, a getter
//! put over a field, a method replaced, a prototype swapped, an own function
//! shadowing a method and removed again); the same sites in functions the
//! loop calls, with a string, a number, an array, a Map and a function
//! among the receivers; and the errors of such sites (a method one layout
//! lacks, `null`, a getter that throws). In the interpreter with the runner
//! entering as it does by itself, from every loop's first back-edge
//! (`ZIPP_LOOP_RUNNER_EAGER`) with and without the lite loop and without
//! frames of its own (`ZIPP_NO_LOOP_FRAMES`), and switched off; by default;
//! and with every function compiled at once.

const SOURCE: &str = r##"// Property reads, writes and method calls in hot loops at sites several object layouts reach: two to six
// layouts, forty, dictionaries; own data, inherited data, accessors and absences mixed at one site; layouts and
// prototypes changed under the loop; in the loop's own frame and in functions it calls.
var out = [];

// 1. One read site over four classes whose field sits at different slots, and one method site over them.
(function () {
  class A { constructor(v) { this.v = v; } get twice() { return this.v * 2; } m(x) { return this.v + x; } }
  class B { constructor(v) { this.pad = 0; this.v = v; } get twice() { return this.v * 2 + 1; } m(x) { return this.v - x; } }
  class C extends A { constructor(v) { super(v); this.extra = 1; } m(x) { return super.m(x) * 2; } }
  class D { constructor(v) { this.a = 1; this.b = 2; this.v = v; } m(x) { return (this.v ^ x) + this.a + this.b; } }
  D.prototype.twice = 7;
  var objs = [new A(1), new B(2), new C(3), new D(4)];
  var s = 0, t = 0, u = 0;
  for (var i = 0; i < 6000; i++) { var o = objs[i & 3]; s = (s + o.v) | 0; t = (t + o.m(i & 15)) | 0; u = (u + o.twice) | 0; }
  out.push("classes " + s + " " + t + " " + u);
})();

// 2. A read site where the name is own data, inherited data, a getter, and missing, by layout.
(function () {
  var proto = { k: 100 };
  var own = { k: 1 }, inh = Object.create(proto), acc = { get k() { return this.z + 5; }, z: 10 }, none = { q: 1 };
  var deep = Object.create(Object.create(Object.create({ k: 1000 })));
  var objs = [own, inh, acc, none, deep, { j: 0, k: 2 }];
  var s = 0, undef = 0;
  for (var i = 0; i < 6000; i++) { var v = objs[i % 6].k; if (v === undefined) undef++; else s = (s + v) | 0; }
  out.push("mixed " + s + " " + undef);
})();

// 3. A write site: an existing slot in three layouts, an append (a new property), a setter, a frozen object in
// sloppy code, a prototype's setter and a read-only inherited property.
(function () {
  var hits = 0;
  var a = { x: 0 }, b = { p: 1, x: 0 }, c = { p: 1, q: 2, x: 0 };
  var setter = { set x(v) { hits += v; }, get x() { return hits; } };
  var frozen = Object.freeze({ x: -1 });
  var viaProto = Object.create({ set x(v) { hits += 2 * v; } });
  var readOnly = Object.create(Object.defineProperty({}, "x", { value: 9, writable: false }));
  var objs = [a, b, c, setter, frozen, viaProto, readOnly];
  for (var i = 0; i < 7000; i++) { objs[i % 7].x = i & 7; }
  var fresh = 0;
  for (var j = 0; j < 3000; j++) { var n = (j & 1) ? { u: 1 } : { u: 1, w: 2 }; n.added = j; fresh = (fresh + n.added + Object.keys(n).length) | 0; }
  out.push("writes " + a.x + b.x + c.x + " " + hits + " " + frozen.x + " " + Object.keys(viaProto).length + " " + readOnly.x + " " + fresh);
})();

// 4. Forty layouts through one read, one write and one call site; then dictionaries.
(function () {
  var objs = [];
  for (var k = 0; k < 40; k++) { var o = {}; for (var j = 0; j <= k % 10; j++) o["f" + j + "_" + (k % 4)] = j; o.val = k; o.get = function () { return this.val; }; objs.push(o); }
  var s = 0;
  for (var i = 0; i < 8000; i++) { var q = objs[i % 40]; q.val = (q.val + 1) & 255; s = (s + q.val + q.get()) | 0; }
  var dicts = [];
  for (var d = 0; d < 6; d++) { var x = { a: d, b: d + 1, c: d + 2, val: d }; delete x.b; if (d & 1) delete x.a; dicts.push(x); }
  var t = 0;
  for (var m = 0; m < 6000; m++) { var y = dicts[m % 6]; y.val = (y.val + 3) & 127; t = (t + y.val + (y.a === undefined ? 1 : y.a) + y.c) | 0; }
  out.push("many " + s + " " + t);
})();

// 5. Layouts and prototypes changed under the loop: a property added to an object, to its prototype, a getter
// put over a field, a method replaced, a prototype swapped, an own property shadowing a method.
(function () {
  function P() { this.v = 1; }
  P.prototype.m = function () { return this.v; };
  P.prototype.w = 5;
  function Q() { this.v = 2; this.z = 0; }
  Q.prototype.m = function () { return this.v * 10; };
  Q.prototype.w = 6;
  var objs = [new P(), new Q(), new P(), new Q()];
  var s = 0;
  for (var i = 0; i < 8000; i++) {
    var o = objs[i & 3];
    s = (s + o.m() + (o.w === undefined ? 3 : o.w) + o.v) | 0;
    if (i === 1000) objs[0].extra = 1;
    if (i === 2000) P.prototype.w = 50;
    if (i === 3000) Object.defineProperty(objs[1], "v", { get: function () { return 7; }, configurable: true });
    if (i === 4000) Q.prototype.m = function () { return -this.v; };
    if (i === 5000) Object.setPrototypeOf(objs[2], Q.prototype);
    if (i === 6000) objs[3].m = function () { return 1000; };
    if (i === 7000) { delete objs[3].m; delete P.prototype.w; }
  }
  out.push("changes " + s);
})();

// 6. The same sites inside functions the loop calls, with primitive and odd receivers among the objects.
(function () {
  function readV(o) { return o.v; }
  function callM(o, x) { return o.m(x); }
  function writeV(o, x) { o.v = x; return o.v; }
  String.prototype.m = function (x) { return this.length + x; };
  Number.prototype.m = function (x) { return this + x; };
  var objs = [{ v: 1, m(x) { return x + 1; } }, { m(x) { return x + 2; }, v: 2 }, "str", 5, [1, 2, 3], new Map([[1, 2]]), function f() {}];
  objs[4].m = function (x) { return this.length * x; }; objs[5].m = function (x) { return this.size - x; }; objs[6].m = function (x) { return x; };
  objs[4].v = 40; objs[5].v = 50; objs[6].v = 60;
  var s = 0, t = 0, u = 0;
  for (var i = 0; i < 7000; i++) { var o = objs[i % 7]; var r = readV(o); s = (s + (r === undefined ? -1 : r)) | 0; t = (t + callM(o, i & 7)) | 0; if (typeof o === "object" && !(o instanceof Map)) u = (u + writeV(o, i & 31)) | 0; }
  delete String.prototype.m; delete Number.prototype.m;
  out.push("frames " + s + " " + t + " " + u);
})();

// 7. Errors at such sites: a missing method on one layout of several, a null among the objects, a throwing getter.
(function () {
  var objs = [{ m() { return 1; }, v: 1 }, { v: 2, m() { return 2; } }, { v: 3 }, null, { get v() { throw new RangeError("g"); }, m() { return 4; } }];
  var s = 0, errs = "";
  for (var i = 0; i < 5000; i++) {
    var o = objs[i % 5];
    try { s = (s + o.m()) | 0; } catch (e) { if (i < 10) errs += e.constructor.name[0]; }
    try { s = (s + o.v) | 0; } catch (e) { if (i < 10) errs += e.constructor.name[0].toLowerCase(); }
  }
  out.push("errors " + s + " " + errs);
})();

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "classes 15000 54000 30000\nmixed 1118000 1000\nwrites 123 10500 -1 0 9 4506000\nmany 1920000 413472\nchanges 449002\nframes 154423 36500 46436\nerrors 13000 TTtrTTtr";

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
fn loop_shapes_worker() {
    if std::env::var_os("ZIPP_LOOP_SHAPES_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn loop_shapes_match_node() {
    if std::env::var_os("ZIPP_LOOP_SHAPES_WORKER").is_some() {
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
            .args(["loop_shapes_worker", "--exact", "--nocapture"])
            .env("ZIPP_LOOP_SHAPES_WORKER", "1");
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
