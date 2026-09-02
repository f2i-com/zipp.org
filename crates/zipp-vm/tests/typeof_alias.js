// typeof-alias oracle: `var t = typeof v; … t === "lit"` must answer exactly
// what the stored string answers, whatever happened to `t` or `v` in between.
// Every case is one where a stale alias would give a different answer. The
// file is SLOPPY on purpose (mapped `arguments`); one case opts into strict.
var out = [];
function rec(label, v) { out.push(label + ":" + v); }
// Run a case enough times to cross the JIT thresholds and demand one answer.
function hot(f) {
  var first = f();
  for (var i = 0; i < 3000; i++) { if (f() !== first) return "DIVERGED@" + i; }
  return first;
}

// 1. the walk shape over every typeof class, in every polarity/looseness
function classify(v) {
  var t = typeof v;
  if (t === "number") return "N";
  if (t === "string") return "S";
  if (t === "boolean") return "B";
  if (t === "undefined") return "U";
  if (t === "object") return v === null ? "0" : "O";
  if (t === "function") return "F";
  if (t === "symbol") return "Y";
  if (t === "bigint") return "I";
  return "?";
}
var vals = [1, 2.5, NaN, -0, "s", "", true, false, undefined, null, {}, [], function () {},
  class C {}, Symbol("k"), 10n, Math, new Date(0), /re/, new Proxy({}, {}),
  new Proxy(function () {}, {}), Infinity, 1e300, Object(1), Object("s")];
rec("1", hot(function () { var s = ""; for (var i = 0; i < vals.length; i++) s += classify(vals[i]); return s; }));
function forms(v) {
  var t = typeof v;
  return [t == "number", t != "number", t !== "string", "number" === t, "string" == t,
    t === "nope", t !== "nope", t == "Number"].join("");
}
rec("2", hot(function () { return forms(1) + "|" + forms("s") + "|" + forms(null); }));

// 3-4. a write to either name between the declaration and the test
function reV(v) { var t = typeof v; v = "s"; return t === "number"; }
function reT(v) { var t = typeof v; t = "string"; return t === "string"; }
function reVcat(v) { var t = typeof v; v += "x"; return t === "number"; }
function reVdes(v) { var t = typeof v; [v] = ["s"]; return t === "number"; }
function reVobj(v) { var t = typeof v; ({ v } = { v: "s" }); return t === "number"; }
function reVinc(v) { var t = typeof v; v++; return t === "string"; }
rec("3", hot(function () { return [reV(1), reT(1), reVcat(1), reVdes(1), reVobj(1), reVinc("7")].join(); }));

// 5. a declaration inside one branch does not reach the code after it
function branch(v, c) { if (c) { var t = typeof v; } return t === "number"; }
function branch2(v, c) { if (c) var t = typeof v; else var t = "x"; return t === "number"; }
rec("5", hot(function () { return [branch(1, false), branch(1, true), branch2(1, false), branch2(1, true)].join(); }));

// 6-7. loops: a write later in the body reaches an earlier use next time
function loopOuter(v) { var t = typeof v; var r = []; for (var i = 0; i < 3; i++) { r.push(t === "number"); v = "s"; } return r.join(); }
function loopInner() { var v = 1; var r = []; while (r.length < 3) { var t = typeof v; r.push(t === "number"); v = "x"; } return r.join(); }
function loopDo() { var v = 1, n = 0; do { var t = typeof v; v = "s"; n++; } while (t === "number" && n < 5); return n; }
function loopForInit(v) { var r = []; for (var t = typeof v; r.length < 2; v = "s") r.push(t === "number"); return r.join(); }
function loopForIn(o) { var v = 1; var t = typeof v; var r = []; for (var k in o) { r.push(t === "number"); v = k; } return r.join(); }
function loopForOf(a) { var v = 1; var t = typeof v; var r = []; for (var x of a) { r.push(t === "number"); v = x; } return r.join(); }
rec("6", hot(function () { return [loopOuter(1), loopInner(), loopDo(), loopForInit(1), loopForIn({ a: 1, b: 2 }), loopForOf(["p", "q"])].join("|"); }));

// 8. switch: a case entered directly never ran the earlier case's declaration
function sw(x, v) { switch (x) { case 1: var t = typeof v; case 2: return t === "number"; } return "-"; }
function sw2(x, v) { switch (x) { case 1: { var t = typeof v; return t === "number"; } default: return t === "undefined"; } }
rec("8", hot(function () { return [sw(1, 5), sw(2, 5), sw(3, 5), sw2(1, 5), sw2(9, 5)].join(); }));

// 9. try/catch/finally around a write; a labelled block with a break
function tc(v) { var t = typeof v; try { v = "s"; throw 0; } catch (e) { return t === "number"; } }
function tf(v) { var t = typeof v; try { return 1; } finally { v = "s"; if (t !== "number") return "bad"; } }
function lb(v) { L: { var t = typeof v; if (v) break L; v = "s"; } return t === "number"; }
function tcNoDef(v, c) { try { if (c) throw 0; var t = typeof v; } catch (e) { return t === "number"; } return "-"; }
function tfNoDef(v, c) { try { if (c) throw 0; var t = typeof v; } catch (e) { } finally { return t === "number"; } }
rec("9", hot(function () { return [tc(1), tf(1), lb(1), lb(0), tcNoDef(1, true), tcNoDef(1, false), tfNoDef(1, true), tfNoDef(1, false)].join(); }));

// 10. closures and eval reach the locals through cells; `with` shadows a name
function clT(v) { var t = typeof v; var g = function () { t = "string"; }; g(); return t === "string"; }
function clV(v) { var t = typeof v; var g = function () { v = "s"; }; g(); return t === "number"; }
function ev(v) { var t = typeof v; eval("v = 's'"); return t === "number"; }
function ev2(v) { var t = typeof v; eval("t = 'q'"); return t === "number"; }
function wi(v, o) { var t = typeof v; with (o) { return t === "number"; } }
function ns(v) { var t = typeof v; function inner(v) { return t === "number"; } return inner("s"); }
rec("10", hot(function () { return [clT(1), clV(1), ev(1), ev2(1), wi(1, { t: "string" }), wi(1, {}), ns(1)].join(); }));

// 11. mapped arguments (sloppy) write a parameter without an instruction; strict does not map
function mapped(v) { var t = typeof v; arguments[0] = "s"; return t === "number"; }
function mappedT(v, t) { var t = typeof v; arguments[1] = "s"; return t === "string"; }
function strictArgs(v) { "use strict"; var t = typeof v; arguments[0] = "s"; return t === "number"; }
rec("11", hot(function () { return [mapped(1), mappedT(1), strictArgs(1)].join(); }));

// 12. `t` is still read as a value: its TypeOf must survive
function used(v) { var t = typeof v; return (t === "number") + ":" + t; }
function usedLater(v) { var t = typeof v; if (t === "string") return "s"; return t.length; }
function cmpVar(v, s) { var t = typeof v; return t === s; }
function lexical(v) { const t = typeof v; let u = typeof v; return [t === "number", u !== "number"].join(); }
function redecl(v, w) { var t = typeof v; var t = typeof w; return t === "string"; }
function selfRe(v) { var v = typeof v; return v === "string"; }
rec("12", hot(function () { return [used(1), usedLater(1), cmpVar(1, "number"), lexical(1), redecl(1, "s"), selfRe(1)].join("|"); }));

// 13. generators suspend with the locals intact; `arguments` and undeclared names
function* gen(v) { var t = typeof v; yield 1; yield t === "number"; v = "s"; yield t === "number"; }
function ta() { var t = typeof arguments; return t === "object"; }
function ug() { var t = typeof notDefinedAnywhere; return t === "undefined"; }
function pr() { var p = Proxy.revocable(function () {}, {}); var v = p.proxy; var t = typeof v; p.revoke(); return (t === "function") + "" + (typeof v === "function"); }
rec("13", hot(function () { var g = gen(1); g.next(); var a = g.next().value, b = g.next().value; return [a, b, ta(), ug(), pr()].join(); }));

// 14. deopt-shaped inputs at a hot fused site: the classes change under the JIT
function poly(v) { var t = typeof v; return (t === "number" ? 1 : 0) + (t === "boolean" ? 2 : 0) + (t === "undefined" ? 4 : 0) + (t === "object" ? 8 : 0) + (t === "string" ? 16 : 0); }
rec("14", hot(function () { var s = 0; for (var i = 0; i < vals.length; i++) s = s * 31 + poly(vals[i]); return s; }));

console.log(out.join("\n"));
