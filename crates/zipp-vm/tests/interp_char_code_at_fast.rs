//! Semantic and diagnostic parity for the interpreter's flat-string + Int
//! `charCodeAt` leaf. The semantic probe runs in an isolated child because
//! builtin statistics are process-wide.

use std::process::Command;

const SOURCE: &str = r#"
"use strict";

function code(text, index) { return text.charCodeAt(index); }

// UTF-16 units (including both halves of a surrogate pair), both OOB sides.
console.log("units", code("A\uD83D\uDE00Z", 0), code("A\uD83D\uDE00Z", 1),
            code("A\uD83D\uDE00Z", 2), code("A\uD83D\uDE00Z", 3),
            code("A\uD83D\uDE00Z", 4), code("A\uD83D\uDE00Z", -1));

// Every non-Int argument must retain the generic ToInteger path and its
// observable coercion. argc=0 likewise remains generic.
var events = [];
var coercible = { valueOf: function () { events.push("valueOf"); return 1.9; } };
console.log("coerce", code("abc", coercible), code("abc", "2"),
            code("abc", 1.9), "abc".charCodeAt(), events.join("|"));

// Warm the intrinsic memo before mutating the live slot: version + slot +
// Value-bits validation must reject replacements, accessors and deletion, then
// accept the restored descriptor again.
var descriptor = Object.getOwnPropertyDescriptor(String.prototype, "charCodeAt");
var warm = 0;
for (var i = 0; i < 32; i++) warm += code("abcd", i & 3);
String.prototype.charCodeAt = function (index) { return 700 + index; };
console.log("replace", warm, code("abc", 2));

var getterHits = 0;
Object.defineProperty(String.prototype, "charCodeAt", {
  configurable: true,
  get: function () {
    getterHits++;
    return function (index) { return 800 + index; };
  }
});
console.log("accessor", code("abc", 1), getterHits);

delete String.prototype.charCodeAt;
var deleted;
try { code("abc", 0); deleted = "missed"; }
catch (error) { deleted = error.constructor.name; }
console.log("deleted", deleted);
Object.defineProperty(String.prototype, "charCodeAt", descriptor);
console.log("restored", code("abc", 2));

// Receiver shapes that can carry their own properties or need flattening stay
// on the generic route.
var boxed = new String("abc");
console.log("boxed", boxed.charCodeAt(1));
var proxyHits = 0;
var proxied = new Proxy(new String("abc"), {
  get: function (target, key) {
    if (key === "charCodeAt") {
      proxyHits++;
      return function (index) { return 900 + index; };
    }
    return target[key];
  }
});
console.log("proxy", proxied.charCodeAt(1), proxyHits);
function makeRope(left) { return left + "cd"; }
console.log("rope", makeRope("ab").charCodeAt(2));

// Primitive member lookup inside a child realm must use that realm's live
// String prototype rather than the main-realm intrinsic proof.
var child = $262.createRealm().global;
child.eval(`
  var childHits = 0;
  String.prototype.charCodeAt = function (index) {
    childHits++;
    return 1000 + index;
  };
  function childCode(text, index) { return text.charCodeAt(index); }
  this.childResult = childCode("abc", 1) + ":" + childHits;
`);
console.log("realm", child.childResult);
"#;

const EXPECTED: &[&str] = &[
    "units 65 55357 56832 90 NaN NaN",
    "coerce 98 99 98 97 valueOf",
    "replace 3152 702",
    "accessor 801 1",
    "deleted TypeError",
    "restored 99",
    "boxed 98",
    "proxy 901 1",
    "rope 99",
    "realm 1001:1",
];

#[test]
fn interp_char_code_at_semantic_worker() {
    if std::env::var_os("ZIPP_INTERP_CHAR_CODE_AT_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, EXPECTED);

    let calls: u64 = zipp_vm::builtin_stats()
        .into_iter()
        .filter(|(_, name, _)| name == "charCodeAt")
        .map(|(_, _, calls)| calls)
        .sum();
    assert_eq!(
        calls, 50,
        "fast and generic paths must count every call once"
    );
}

#[test]
fn interp_char_code_at_fast_matches_expected_semantics() {
    if std::env::var_os("ZIPP_INTERP_CHAR_CODE_AT_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let out = Command::new(&exe)
        .args([
            "interp_char_code_at_semantic_worker",
            "--exact",
            "--nocapture",
        ])
        .env("ZIPP_INTERP_CHAR_CODE_AT_WORKER", "1")
        .env("ZIPP_NOJIT", "1")
        .env("ZIPP_BUILTINSTATS", "1")
        .output()
        .expect("spawn semantic worker");
    assert!(
        out.status.success(),
        "fast interpreter mode diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
