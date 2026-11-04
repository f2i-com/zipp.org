//! A builtin's leaf callback (a sort comparator, an array method's callback)
//! runs without a frame in a VM that runs only the interpreter
//! (`Vm::leaf_callback`), and every way such a run declines takes the frame:
//! mixed types, a captured write, `arguments.length`, `this` (strict, sloppy,
//! object, lexical), a class, a bound function, a getter, a throw, a global
//! replaced mid-run, holey arrays and array-likes. Node's output, run
//! interpreted, interpreted without leaf callbacks, and at the defaults.

const CHILD: &str = "ZIPP_LEAF_CALLBACKS_CHILD";

#[test]
fn leaf_callbacks_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn leaf_callbacks_match_node() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, envs) in [
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("interpreter-no-leaf", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_LEAF_CALLBACK", "1")][..]),
        ("defaults", &[][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "leaf_callbacks_child", "--nocapture"])
            .env(CHILD, "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_LEAF_CALLBACK");
        for (k, v) in envs {
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
const EXPECTED: &str = r####"sum=21395
mixed=,1,2.5,3,4,7,10
strs=apple,fig,kiwi,pear
objs=1,2,3
calls=30
argc=3,3,3
strictThis=number,number
sloppyThis=true,true
objThis=11,12
arrowThis=9,9
class=TypeError
bound=101,102,103
getter=5,6
throw=two
global=2,2s,3s,4s
holey=33,3267
like=6,,8,"####;

const PROGRAM: &str = r####"// Leaf callbacks of builtins (sort comparators, array methods) in an
// interpreter-only VM: every way a leaf run declines, against Node.
var out = [];
function show(label, v) { out.push(label + "=" + (Array.isArray(v) ? v.join(",") : String(v))); }
function nums(n, k) { var a = []; for (var i = 0; i < n; i++) a.push(((i * 7919 + k) % 101) - 50); return a; }
var sum = 0;
for (var rep = 0; rep < 300; rep++) {
  var a = nums(40, rep);
  a.sort(function (x, y) { return x - y; });
  var b = nums(40, rep).sort((x, y) => (x < y ? 1 : x > y ? -1 : 0));
  var c = nums(40, rep).map((x) => x * 2 + 1).filter((x) => x > 0);
  var d = nums(40, rep).reduce((acc, x) => acc + x, 0);
  var e = nums(40, rep).some((x) => x === 49) + nums(40, rep).every((x) => x > -51) * 2;
  sum = (sum + a[0] + a[39] + b[0] + c.length + d + e) | 0;
}
show("sum", sum);
// Mixed types: the comparator declines and runs in a frame (a consistent order).
show("mixed", [3, "10", 2.5, "4", 1, null, 7].sort(function (x, y) { return x - y; }));
show("strs", ["pear", "apple", "fig", "kiwi"].sort((x, y) => (x < y ? -1 : x > y ? 1 : 0)));
show("objs", [{ k: 3 }, { k: 1 }, { k: 2 }].sort((x, y) => x.k - y.k).map((o) => o.k));
// A capturing callback that writes its variable, and one that reads `arguments.length`.
var calls = 0;
nums(30, 1).forEach(function (x) { calls = calls + 1; });
show("calls", calls);
show("argc", [1, 2, 3].map(function () { return arguments.length; }));
// `this`: strict with a primitive, sloppy with undefined (global) and with an object.
show("strictThis", [1, 2].map(function () { "use strict"; return typeof this; }, 5));
show("sloppyThis", [1, 2].map(function () { return this === globalThis; }));
show("objThis", [1, 2].map(function (x) { return this.k + x; }, { k: 10 }));
show("arrowThis", (function () { return [1, 2].map(() => this.v); }).call({ v: 9 }));
// Not plain functions: a class throws, a bound function and a getter-backed read.
try { [1].map(class C {}); } catch (e) { show("class", e.constructor.name); }
show("bound", [1, 2, 3].map(function (x) { return x + this.d; }.bind({ d: 100 })));
var g = { get p() { return 4; } };
show("getter", [1, 2].map(function (x) { return x + g.p; }));
// Throws from a callback; a global it reads replaced mid-run.
try { [1, 2, 3].map(function (x) { if (x === 2) throw new RangeError("two"); return x; }); } catch (e) { show("throw", e.message); }
var K = 1;
show("global", [1, 2, 3, 4].map(function (x) { if (x === 2) K = "s"; return x + K; }));
// Big holey array and array-like, with leaf callbacks.
var h = []; for (var i = 0; i < 100; i++) h.push(i); for (var i = 0; i < 100; i += 3) delete h[i];
show("holey", [h.map((x) => x * 2).filter((x) => x % 4 === 0).length, h.reduce((s, x) => s + x, 0)]);
show("like", Array.prototype.map.call({ length: 4, 0: 5, 2: 7 }, (x) => x + 1));
console.log(out.join("\n"));
"####;
