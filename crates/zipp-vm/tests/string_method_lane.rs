//! Method calls on primitive strings reach the string builtins directly from
//! the interpreter's `CallMethod` arm, and `padStart`/`padEnd`/`repeat` of
//! an ASCII receiver build their result without copying it first. Their
//! answers and their coercion order (the length before the filler, the
//! early returns that skip the filler's `toString`, the RangeErrors) must be
//! Node's, as must every way the lane must decline: a patched, deleted or
//! accessor prototype method, the `toString`/`valueOf` family, a rope
//! receiver, non-ASCII receivers and fillers, and calls with many arguments.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name; } }
var q = function (s) { return JSON.stringify(s); };
var order = [];
var len = function (n) { return { valueOf: function () { order.push("len" + n); return n; } }; };
var fill = function (s) { return { toString: function () { order.push("fill" + s); return s; } }; };

var bases = ["", "a", "abc", "abcdef", "héllo", "\u{1F600}x"];
for (var b of bases) {
  log("pad", q(b), q(b.padStart(5)), q(b.padEnd(5)), q(b.padStart(8, "0")), q(b.padEnd(8, "ab")),
    q(b.padStart(7, "")), q(b.padEnd(2, "zz")), q(b.padStart(9, "é")), q(b.padEnd(6, "\u{1F600}")),
    q(b.padStart(-3, "x")), q(b.padEnd(NaN)), q(b.padStart(6.9, "-")), q(b.padStart(6, 12)), q(b.padEnd(6, null)));
  log("repeat", q(b.repeat(0)), q(b.repeat(1)), q(b.repeat(3)), q(b.repeat(NaN)), q(b.repeat(2.7)),
    catchName(function () { return b.repeat(-1); }), catchName(function () { return b.repeat(Infinity); }));
}
log("huge", catchName(function () { return "a".padStart(2 ** 40); }), catchName(function () { return "ab".repeat(2 ** 40); }),
  catchName(function () { return "".repeat(Infinity); }), q("".repeat(2 ** 40)));

// Coercion order: the length first, the filler only when padding happens.
order = [];
log("order", q("ab".padStart(len(5), fill("xy"))), q("ab".padEnd(len(1), fill("never"))), q("abcd".padStart(len(4), fill("no"))),
  q("ab".repeat(len(2))), order.join(","));
log("order-throw", catchName(function () { return "ab".padStart(5, Symbol("s")); }), q("ab".padStart(1, Symbol("s"))),
  catchName(function () { return "ab".padStart({ valueOf: function () { throw new RangeError("v"); } }, "x"); }));

// A loop hot enough for every tier, on rope and flat receivers.
var acc = 0;
for (var i = 0; i < 3000; i++) {
  var s = String(i * 7919);
  var r = "<" + s + ">";
  acc = (acc + s.padStart(8, "0").length + r.padEnd(12, ".").charCodeAt(11) + s.repeat(1 + (i & 3)).length
    + r.trim().length + r.at(-1).charCodeAt(0) + r.slice(1, -1).length + r.indexOf(">")) | 0;
}
log("loop", acc);

// Many arguments, and the deferred family.
log("many", "a".concat(1, 2, 3, 4, 5, 6), "x,y".split(",", 1, 2, 3, 4, 5).length, "s".toString(), "s".valueOf(), q("s".toLocaleString()));

// Patched, then restored; deleted; an accessor; a method only some strings reach.
var realPad = String.prototype.padStart, realAt = String.prototype.at, realTrim = String.prototype.trim;
String.prototype.padStart = function (n) { return "patched:" + this + ":" + n; };
log("patched", "ab".padStart(5), ("a" + "b").padStart(3));
String.prototype.padStart = realPad;
log("restored", q("ab".padStart(5)));
delete String.prototype.at;
log("deleted", catchName(function () { return "abc".at(0); }));
String.prototype.at = realAt;
Object.defineProperty(String.prototype, "trim", { get: function () { return function () { return "accessor"; }; }, configurable: true });
log("accessor", "  x ".trim());
Object.defineProperty(String.prototype, "trim", { value: realTrim, writable: true, configurable: true });
log("trim-restored", q("  x ".trim()));
String.prototype.shout = function () { return this.toUpperCase() + "!"; };
log("user-method", "hey".shout(), ("he" + "y").shout());
log("generic", String.prototype.padStart.call(12, 5, "0"), catchName(function () { return String.prototype.repeat.call(null, 2); }));
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
fn string_method_lane_worker() {
    if std::env::var_os("ZIPP_STRING_LANE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

#[test]
fn string_method_lane_matches_node() {
    if std::env::var_os("ZIPP_STRING_LANE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit) in [("default", false), ("interpreter", true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["string_method_lane_worker", "--exact", "--nocapture"])
            .env("ZIPP_STRING_LANE_WORKER", "1")
            .env_remove("ZIPP_NOJIT");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
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
