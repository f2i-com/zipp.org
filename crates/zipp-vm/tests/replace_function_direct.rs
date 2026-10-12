//! `String.prototype.replace(RegExp, function)` through the internal matcher.
//!
//! `RegExp.prototype[Symbol.replace]` collects every match through its exec
//! loop before it calls a functional replacer once, so a replacer cannot
//! reach the matcher: whatever it does to `lastIndex`, to
//! `RegExp.prototype.exec` or to the regex itself happens after the last
//! exec. For a plain regex (the guard the string-template lane already
//! uses) the internal matcher's calls `(match, ...captures, position, input
//! [, groups])` are therefore the protocol's. The protocol built an exec
//! result array per match and read five properties of it back, four times
//! what Node pays; a callable replacement now takes the direct lane a
//! primitive-string one takes.
//!
//! The lines pin what a replacer is given and can observe: its arguments
//! (undefined captures, named groups, positions in UTF-16 units), how its
//! result is coerced and in what order, a throw, `lastIndex` and the legacy
//! statics inside it, a replacer that patches and deletes `exec`, the
//! regexes that must still take the full protocol (sticky, a set
//! `lastIndex`, an own `exec`, a subclass), every kind of callable, and a
//! compiled capturing replacer called thousands of times, nested.
//!
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Differential probe: String.prototype.replace(RegExp, function) through every observable point.
var out = [];
function log(label, v) { out.push(label + ": " + v); }
function show(a) { return Array.prototype.map.call(a, function (x) { return x === undefined ? "u" : typeof x === "object" ? JSON.stringify(x) : String(x); }).join("|"); }

// 1. The arguments: match, captures (some undefined), position, input, groups.
log("args", "a1b22c".replace(/([a-z])(\d+)?(x)?/g, function () { return "[" + show(arguments) + "]"; }));
log("named", "2024-05".replace(/(?<y>\d{4})-(?<m>\d\d)/, function () { var g = arguments[arguments.length - 1]; return g.m + "/" + g.y + ":" + (Object.getPrototypeOf(g) === null) + ":" + arguments.length; }));
log("non-global", "aXbXc".replace(/X/, function (m, p) { return "<" + p + ">"; }));
log("no match", "abc".replace(/z/g, function () { return "!"; }));
log("empty matches", "abc".replace(/x*/g, function (m, p) { return "(" + p + ")"; }));
log("unicode", "a😀b\uD83D".replace(/[^a-z]/gu, function (m, p) { return m.length + "@" + p; }) + " " + "a😀b".replace(/[^a-z]/g, function (m, p) { return m.length + "@" + p; }));

// 2. What the replacer returns: coerced with ToString, in order.
var order = [];
log("return kinds", "abcd".replace(/[a-d]/g, function (m, p) {
  return [1.5, null, undefined, { toString: function () { order.push("ts" + p); return "<o>"; } }][p];
}) + " " + order.join(","));
try { "ab".replace(/a/g, function () { return Symbol("s"); }); log("symbol", "no throw"); } catch (e) { log("symbol", e.name); }
try { "abc".replace(/[a-c]/g, function (m, p) { order.push("cb" + p); if (p === 1) throw new RangeError("stop"); return m; }); } catch (e) { log("throw", e.name + " " + order.join(",")); }

// 3. What the replacer can see: lastIndex, the legacy statics, the regex.
var re = /(b)(c)?/g;
log("state inside", "abcab".replace(re, function (m, p1, p2, pos) { return "{" + re.lastIndex + "," + RegExp.lastMatch + "," + RegExp.$1 + "," + (RegExp.$2 === "" ? "e" : RegExp.$2) + "," + RegExp.leftContext + "}"; }) + " after " + re.lastIndex);
var origExec = RegExp.prototype.exec;
var re2 = /a/g, seen = [];
log("mutations inside", "aaaa".replace(re2, function (m, p) {
  if (p === 0) { re2.lastIndex = 3; RegExp.prototype.exec = function () { seen.push("patched"); return null; }; }
  if (p === 1) delete RegExp.prototype.exec;
  return p;
}) + " " + seen.length + " " + re2.lastIndex);
Object.defineProperty(RegExp.prototype, "exec", { value: origExec, writable: true, configurable: true, enumerable: false });
log("exec restored", typeof /a/.exec("a") + " " + "xax".replace(/a/g, function () { return "-"; }));

// 4. A regex that must take the full protocol: sticky, lastIndex set, own exec, subclass, patched prototype.
var sticky = /a/y; sticky.lastIndex = 1;
log("sticky", "aaba".replace(sticky, function (m, p) { return "[" + p + "]"; }) + " " + sticky.lastIndex);
var li = /a/g; li.lastIndex = 2;
log("lastIndex set", "aaa".replace(li, function (m, p) { return p; }) + " " + li.lastIndex);
var own = /a/g; own.exec = function (s) { var r = RegExp.prototype.exec.call(this, s); if (r) r[0] = "Z"; return r; };
log("own exec", "aa".replace(own, function (m, p) { return m + p; }));
class Sub extends RegExp { exec(s) { var r = super.exec(s); if (r) r.index = 0; return r; } }
log("subclass", "xaya".replace(new Sub("a", "g"), function (m, p) { return "<" + p + ">"; }));

// 5. Every kind of callable: bound, native, proxy, class (throws), arrow with captured state.
function tag(m) { return this === undefined ? "[" + m + "]" : "(" + this.t + m + ")"; }
log("bound", "ab".replace(/[ab]/g, tag.bind({ t: "#" })) + " " + "ab".replace(/[ab]/g, function (m) { "use strict"; return this === undefined ? "u" : "o"; }) + " " + "ab".replace(/[ab]/g, function (m) { return this === globalThis ? "g" : "o"; }));
log("native", "a,b".replace(/[ab]/g, String.prototype.toUpperCase.call.bind(String.prototype.toUpperCase)) + " " + "3 4".replace(/\d/g, Math.sqrt));
var traps = [];
log("proxy", "ab".replace(/[ab]/g, new Proxy(function (m) { return m + m; }, { apply: function (t, th, args) { traps.push(args[0] + args[1]); return t.apply(th, args); } })) + " " + traps.join(","));
try { "a".replace(/a/g, class K {}); log("class", "no throw"); } catch (e) { log("class", e.name); }

// 6. Hot: a compiled capturing replacer called many times, results checked.
function digits(s, k) { return s.replace(/[0-9]/g, function (d, p) { var t; if (d > "4") t = String(+d + k); return t === undefined ? "_" + (p % 3) : t; }); }
var acc = 0;
for (var i = 0; i < 3000; i++) acc = (acc + digits("a1b7c3d9e5f0", i & 3).length) | 0;
log("hot", acc + " " + digits("a1b7c3d9e5f0", 2) + " " + digits("0123456789", 10));
function nested(s) { return s.replace(/\w+/g, function (w) { return w.replace(/[aeiou]/g, function (v) { return v.toUpperCase(); }); }); }
for (var i = 0; i < 2000; i++) nested("hello wide world");
log("nested", nested("hello wide world") + " " + RegExp.lastMatch);

console.log(out.join("\n"));
"####;

#[test]
fn replace_with_a_function_matches_node() {
    let expected: &[&str] = &[
        r####"args: [a1|a|1|u|0|a1b22c][b22|b|22|u|2|a1b22c][c|c|u|u|5|a1b22c]"####,
        r####"named: 05/2024:true:6"####,
        r####"non-global: a<1>bXc"####,
        r####"no match: abc"####,
        r####"empty matches: (0)a(1)b(2)c(3)"####,
        r####"unicode: a2@1b1@4 a1@11@2b"####,
        r####"return kinds: 1.5nullundefined<o> ts3"####,
        r####"symbol: TypeError"####,
        r####"throw: RangeError ts3,cb0,cb1"####,
        r####"state inside: a{0,b,b,e,abca}a{0,b,b,e,abca} after 0"####,
        r####"mutations inside: 0123 0 3"####,
        r####"exec restored: object x-x"####,
        r####"sticky: a[1]ba 2"####,
        r####"lastIndex set: 012 0"####,
        r####"own exec: Z0Z1"####,
        r####"subclass: <0>aya"####,
        r####"bound: (#a)(#b) uu gg"####,
        r####"native: A,B 1.7320508075688772 2"####,
        r####"proxy: aabb a0,b1"####,
        r####"class: TypeError"####,
        r####"hot: 48000 a_1b9c_2d11e7f_2 _0_1_2_0_11516171819"####,
        r####"nested: hEllO wIdE wOrld o"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
