// Run against Lite or a full artifact through pkg-redirect.cjs / ZIPP_PKG.
"use strict";
const assert = require("node:assert/strict");
const { Engine, zippProfile } = require("./pkg/zipp_wasm.js");
const profile = JSON.parse(zippProfile());
const lite = profile.variant === "lite";
assert.ok(["lite", "full"].includes(profile.variant));
assert.deepEqual(profile.omittedFeatures, lite ? ["Intl", "Temporal", "Python", "Torch"] : []);
if (lite) assert.deepEqual(profile.languages, ["javascript"]);
const e = new Engine();
try {
  e.initScript(String.raw`
    function check(ok, name) { if (!ok) throw new Error(name); }
    check(typeof Intl === ${JSON.stringify(lite ? "undefined" : "object")}, "Intl presence");
    check(typeof Temporal === ${JSON.stringify(lite ? "undefined" : "object")}, "Temporal presence");
    check(("Intl" in globalThis) === ${!lite}, "Intl global descriptor");
    check(("Temporal" in globalThis) === ${!lite}, "Temporal global descriptor");
    check(("toTemporalInstant" in Date.prototype) === ${!lite}, "Date extension");
    check(eval("21 * 2") === 42, "eval");
    check(new Function("x", "return x + 1")(41) === 42, "Function");
    check((2n ** 130n).toString() === "1361129467683753853853498429727072845824", "BigInt");
    check(/\p{Script=Greek}+/u.test("αβ"), "Unicode property");
    check(/\p{Script=Unknown}/u.test("\ud800"), "Unknown property");
    check(/(?<α>\p{Letter}+)/u.exec("abc").groups.α === "abc", "Unicode group name");
    check(/[\p{ASCII}&&\p{Letter}]/v.test("a"), "Unicode sets");
    check(/\u{1f600}/u.test("😀"), "astral regexp");
    check("e\u0301".normalize() === "é", "normalization");
    check("\ud800".charCodeAt(0) === 0xd800, "lone surrogate");
    check(new Proxy({x: 21}, {get(t, k) {return t[k] * 2;}}).x === 42, "Proxy");
    check(new Map([["x", 42]]).get("x") === 42 && new Set([1,1]).size === 1, "collections");
    const buf = new ArrayBuffer(8), view = new DataView(buf);
    view.setFloat64(0, 42); check(view.getFloat64(0) === 42, "typed memory");
    class Base { x() { return 40; } }
    class Sub extends Base { x() { return super.x() + 2; } }
    check(new Sub().x() === 42, "classes");
    function* gen() { yield 40; yield 2; }
    check([...gen()].reduce((a,b) => a+b) === 42, "generators");
    check(new Date("2026-01-02T03:04:05+01:00").toISOString() === "2026-01-02T02:04:05.000Z", "Date offset parser");
    ${lite ? String.raw`
    const poison = {get length() {throw new Error("locale read");}, get style() {throw new Error("option read");}};
    check((1234.5).toLocaleString(poison, poison) === "1234.5", "Number default locale");
    check((12345678901234567890n).toLocaleString(poison, poison) === "12345678901234567890", "BigInt default locale");
    const d = new Date(0);
    check(d.toLocaleString(poison, poison) === d.toString(), "Date default locale");
    check(d.toLocaleDateString() === d.toDateString(), "Date-only locale");
    check(d.toLocaleTimeString() === d.toTimeString(), "Time-only locale");
    check(new Date(NaN).toLocaleString() === "Invalid Date", "invalid Date");
    check("e\u0301".localeCompare("é", poison, poison) === 0, "canonical comparison");
    check("\ud800".localeCompare("\ufffd") !== 0, "surrogate comparison");
    check("\ud800".localeCompare("\u{dd800}") !== 0, "surrogate versus plane 13");
    check("I".toLocaleLowerCase(poison) === "i", "default lower case");
    check("i".toLocaleUpperCase(poison) === "I", "default upper case");
    ` : String.raw`
    check(new Intl.NumberFormat("en-US").format(1234) === "1,234", "full Intl");
    check(Temporal.PlainDate.from("2026-01-02").year === 2026, "full Temporal");
    check(Temporal.ZonedDateTime.from("2024-01-01T00:00[Australia/Sydney]").offset === "+11:00", "full tzdb");
    `}
    print("variant checks passed");
  `);
  assert.deepEqual(e.takeOutput(), ["variant checks passed"]);
} finally { e.dispose(); e.free(); }
if (lite) {
  const py = new Engine();
  try { assert.throws(() => py.initSource("print(42)", "python"), /python|Python/); }
  finally { py.dispose(); py.free(); }
}
console.log(`${profile.variant} feature contract and JavaScript core passed`);
