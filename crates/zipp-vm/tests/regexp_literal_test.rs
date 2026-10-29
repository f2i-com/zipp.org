//! `/re/.test(x)` with the literal as the direct receiver compiles to
//! `RegExpLitProbe` + `RegExpLitTest`, which run the intrinsic `test` on the
//! literal's shared core without making the object while nothing could see
//! it, and make it whenever something could: a replaced or accessor `test`,
//! an `exec` replaced before or during the call (by the argument's
//! `toString`), a child realm. Results, call orders, receivers, ToString
//! counts, legacy statics and throws are Node 24's (the realm cases, which
//! Node cannot run, follow the spec), in every execution mode, and under the
//! older lowering (`ZIPP_NO_RX_LIT_TEST=1`).

use std::process::Command;

const CHILD_ENV: &str = "ZIPP_REGEXP_LITERAL_TEST_CHILD";

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        out.error,
        out.output
    );
    out.output.iter().flat_map(|l| l.lines()).map(str::to_string).collect()
}

#[test]
fn regexp_literal_test_child() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "plain false",
            "global fresh 120",
            "sticky fresh 120",
            "kinds true",
            "object toString calls 120",
            "statics 12 34 12-34 [x ]",
            "statics hot b c bc",
            "exec swap true calls=1 [object RegExp]:z:zz",
            "exec replaced calls=120 distinct receivers=120",
            "exec bad result TypeError",
            "exec deleted true",
            "test replaced 3 arg,call:s0,arg,call:s1 distinct=120",
            "test restored true",
            "accessor test true get:true:m,arg",
            "throws TypeError RangeError:boom",
            "hot throws 3",
            "shared vs fused 200 lastIndex=0",
            "unicode true true",
            "eval literals bad=0 hits=1040 long bad=0 hits=1040",
        ]
    );
    assert_eq!(run_ok(EVAL_LITERALS), ["fails 0 "]);
    assert_eq!(
        run_ok(REALMS),
        [
            "main 120",
            "child 120",
            "child:true:x119",
            "main still true",
            "child exec false",
            "main exec true",
        ]
    );
}

#[test]
fn regexp_literal_test_in_all_modes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc-stress", &[("ZIPP_GC_STRESS", "1")][..]),
        (
            "interpreter-gc-stress",
            &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
        ("older-lowering", &[("ZIPP_NO_RX_LIT_TEST", "1")][..]),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["--exact", "regexp_literal_test_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env("ZIPP_CACHE_DIR", "")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_RX_LIT_TEST");
        cmd.envs(env.iter().copied());
        let out = cmd.output().expect("spawn execution-mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"
const N = 120;
const out = [];
const log = (s) => out.push(s);

function hot(f) {
  let last;
  for (let i = 0; i < N; i++) last = f(i);
  return last;
}

// 1. Plain: results, and a global/sticky literal is fresh every time.
log("plain " + hot((i) => /[a-z_0-9]/.test(String.fromCharCode(40 + (i % 90)))));
let gTrue = 0;
hot(() => { if (/a/g.test("aa")) gTrue++; });
log("global fresh " + gTrue);
let yTrue = 0;
hot(() => { if (/a/y.test("ab")) yTrue++; });
log("sticky fresh " + yTrue);

// 2. Arguments of every primitive kind, and an object (ToString once).
function kinds(i) {
  const v = [undefined, null, 12, true, "x1", 1.5e300, -0][i % 7];
  return /^(u|n|1|t|x|-?\d)/.test(v);
}
log("kinds " + hot(kinds));
let toStrCalls = 0;
const obj = { toString() { toStrCalls++; return "abc"; } };
hot(() => /b/.test(obj));
log("object toString calls " + toStrCalls);

// 3. Legacy statics after a fused test.
/(\d+)-(\d+)/.test("x 12-34 y");
log("statics " + RegExp.$1 + " " + RegExp.$2 + " " + RegExp.lastMatch + " [" + RegExp.leftContext + "]");
hot((i) => /(b)(c)?/.test("abc" + i));
log("statics hot " + RegExp.$1 + " " + RegExp.$2 + " " + RegExp.lastMatch);

// 4. An argument whose toString replaces exec: the user exec runs once, on
//    a RegExp receiver, with the coerced string.
{
  const origExec = RegExp.prototype.exec;
  let seen = [];
  let calls = 0;
  const sneaky = {
    toString() {
      RegExp.prototype.exec = function (s) {
        calls++;
        seen.push(Object.prototype.toString.call(this) + ":" + this.source + ":" + s);
        return origExec.call(this, s);
      };
      return "zz";
    },
  };
  const r1 = /z/.test(sneaky);
  RegExp.prototype.exec = origExec;
  log("exec swap " + r1 + " calls=" + calls + " " + seen.join(","));
}

// 5. exec replaced before: the user exec runs every time, each time with a
//    distinct fresh receiver.
{
  const origExec = RegExp.prototype.exec;
  const receivers = new Set();
  let n = 0;
  RegExp.prototype.exec = function (s) { n++; receivers.add(this); return origExec.call(this, s); };
  hot((i) => /q/.test("aq" + i));
  RegExp.prototype.exec = origExec;
  log("exec replaced calls=" + n + " distinct receivers=" + receivers.size);
}

// 6. exec returning a non-object: TypeError.
{
  const origExec = RegExp.prototype.exec;
  RegExp.prototype.exec = function () { return 1; };
  let msg = "none";
  try { /a/.test("a"); } catch (e) { msg = e.constructor.name; }
  RegExp.prototype.exec = origExec;
  log("exec bad result " + msg);
}

// 7. exec deleted: the builtin runs.
{
  const d = Object.getOwnPropertyDescriptor(RegExp.prototype, "exec");
  delete RegExp.prototype.exec;
  let r = hot((i) => /k/.test("kk" + i));
  Object.defineProperty(RegExp.prototype, "exec", d);
  log("exec deleted " + r);
}

// 8. test replaced: the user function receives a fresh RegExp each time,
//    read before the argument, called after it; restored, the intrinsic.
{
  const origTest = RegExp.prototype.test;
  const order = [];
  let rec = new Set();
  RegExp.prototype.test = function (s) { order.push("call:" + s); rec.add(this); return this.source.length; };
  const r = hot((i) => /abc/.test((order.push("arg"), "s" + (i % 2))));
  RegExp.prototype.test = origTest;
  log("test replaced " + r + " " + order.slice(0, 4).join(",") + " distinct=" + rec.size);
  log("test restored " + hot(() => /abc/.test("xabcx")));
}

// 9. An accessor test: the getter runs before the argument, on a RegExp.
{
  const d = Object.getOwnPropertyDescriptor(RegExp.prototype, "test");
  const order = [];
  Object.defineProperty(RegExp.prototype, "test", {
    configurable: true,
    get() { order.push("get:" + (this instanceof RegExp) + ":" + this.source); return d.value; },
  });
  const r = /m/.test((order.push("arg"), "m"));
  Object.defineProperty(RegExp.prototype, "test", d);
  log("accessor test " + r + " " + order.join(","));
}

// 10. Throws: a Symbol argument (ToString), a throwing toString, and both
//     interleaved with hot successes.
{
  let a = "none", b = "none";
  try { /a/.test(Symbol("s")); } catch (e) { a = e.constructor.name; }
  try { /a/.test({ toString() { throw new RangeError("boom"); } }); } catch (e) { b = e.constructor.name + ":" + e.message; }
  log("throws " + a + " " + b);
  let caught = 0;
  hot((i) => { try { return /a/.test(i % 50 === 0 ? Symbol() : "a"); } catch (e) { caught++; } });
  log("hot throws " + caught);
}

// 11. A shared global regex beside the fused form of the same literal.
{
  const shared = /o/g;
  let s = 0;
  hot(() => { if (shared.test("oo")) s++; if (/o/g.test("oo")) s++; });
  log("shared vs fused " + s + " lastIndex=" + shared.lastIndex);
}

// 12. Unicode and non-ASCII subjects.
log("unicode " + hot((i) => /\u{1F600}/u.test("x\u{1F600}" + i)) + " " + hot(() => /é/i.test("CAFÉ")));

// 13. Many literals made by eval, each a different pattern, short and long
//    (a long constant is a new string at every load; a freed one's heap
//    slot holds another next): no literal may take another's program.
{
  let bad = 0, hits = 0, longBad = 0, longHits = 0;
  for (let cu = 0; cu < 26 * 40; cu++) {
    const ch = String.fromCharCode(97 + (cu % 26)) + (cu % 7);
    if (eval("/" + ch + "/").source !== ch) bad++;
    if (eval("/" + ch + "$/.test('x" + ch + "')")) hits++;
    const long = "q".repeat(40) + ch;
    if (eval("/" + long + "/").source !== long) longBad++;
    if (eval("/" + long + "/.test('" + long + "')")) longHits++;
  }
  log("eval literals bad=" + bad + " hits=" + hits + " long bad=" + longBad + " hits=" + longHits);
}

console.log(out.join("\n"));
"####;

// Test262's language/literals/regexp/S7.8.5_A1.1_T2.js: a literal of every
// code unit, each made by its own eval. A literal's cache must not be keyed by
// the strings its constants load as: they are made afresh, and their freed
// heap slots taken by others (a bits key gave U+E565 another literal's program).
const EVAL_LITERALS: &str = r####"
var fails = [];
for (var cu = 0; cu <= 0xffff; ++cu) {
  var Elimination =
    ((cu === 0x002A) || (cu === 0x002F) || (cu === 0x005C) || (cu === 0x002B) ||
     (cu === 0x003F) || (cu === 0x0028) || (cu === 0x0029) ||
     (cu === 0x005B) || (cu === 0x005D) || (cu === 0x007B) || (cu === 0x007D));
  var LineTerminator = ((cu === 0x000A) || (cu === 0x000D) || (cu === 0x2028) || (cu === 0x2029));
  if ((Elimination || LineTerminator) === false) {
    var xx = String.fromCharCode(cu);
    var pattern = eval("/" + xx + "/");
    if (pattern.source !== xx && fails.length < 5) fails.push(cu.toString(16));
  }
}
console.log("fails " + fails.length + " " + fails.join(","));
"####;

const REALMS: &str = r####"
const out = [];
const g = $262.createRealm().global;
// Main-realm literals once a child realm exists.
let m = 0;
for (let i = 0; i < 120; i++) if (/a/.test("ba" + i)) m++;
out.push("main " + m);
// A literal evaluated by child-realm code is the child's RegExp.
g.eval("var hits = 0; for (var i = 0; i < 120; i++) if (/a/.test('ba' + i)) hits++;");
out.push("child " + g.hits);
const childTest = g.RegExp.prototype.test;
g.RegExp.prototype.test = function (s) {
  return "child:" + (Object.getPrototypeOf(this) === g.RegExp.prototype) + ":" + s;
};
out.push(g.eval("var r; for (var i = 0; i < 120; i++) r = /a/.test('x' + i); r"));
out.push("main still " + /a/.test("a"));
g.RegExp.prototype.test = childTest;
g.RegExp.prototype.exec = function () { return null; };
out.push("child exec " + g.eval("var e; for (var i = 0; i < 120; i++) e = /a/.test('a'); e"));
out.push("main exec " + /a/.test("a"));
console.log(out.join("\n"));
"####;
