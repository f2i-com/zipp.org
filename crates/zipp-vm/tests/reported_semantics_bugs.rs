//! Semantics bugs reported against `main` at 2cb7176f and a82ad0bb
//! (f2i-com/zipp.org #41 to #50). Every expected line is what Node 24
//! prints for the same source; the hot loops make the compiled paths
//! answer too.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// #41: a class, function or other non-plain object on another object's
/// prototype chain is searched, and its getters see the original receiver.
#[test]
fn class_on_a_function_chain_is_searched() {
    let out = run_ok(
        r#"
        function fn() {}
        class C { static s() { return "static"; } static get g() { return this === fn; } static q = 9; }
        Reflect.setPrototypeOf(fn, C);
        console.log(typeof fn.s, fn.s(), fn.g, fn.q, fn.name, typeof fn.call);
        function g() {} g.x = 5;
        function h() {} Reflect.setPrototypeOf(h, g.bind(null));
        var b = g.bind(null); b.y = 7; Reflect.setPrototypeOf(h, b);
        console.log(h.y, typeof h.apply);
        var m = new Map([[1, 2]]); m.k = 4;
        function k() {} Reflect.setPrototypeOf(k, m);
        console.log(k.k, typeof k.get);
        var n = 0;
        function hot() {} Reflect.setPrototypeOf(hot, C);
        for (var i = 0; i < 3000; i++) if (typeof hot.s === "function") n++;
        console.log(n);
        function t(f) { try { return String(f()); } catch (e) { return e.constructor.name; } }
        console.log(t(() => Object.create(new Map([[1, 2]])).size), t(() => Object.create(new Set([1])).size),
            t(() => Reflect.get(new Map([[1, 1]]), "size", {})));
        var own = new Map([[1, 2]]); Object.defineProperty(own, "size", { value: 99 });
        class M extends Map { get size() { return "sub"; } }
        class M2 extends Map {}
        console.log(own.size, new M([[1, 2]]).size, new M2([[1, 2], [3, 4]]).size);
        "#,
    );
    assert_eq!(
        out,
        [
            "function static true 9 fn function",
            "7 function",
            "4 function",
            "3000",
            "TypeError TypeError TypeError",
            "99 sub 2",
        ]
    );
}

/// #42: `typeof name` inside `with` asks the `with` object first, as a
/// plain read does.
#[test]
fn typeof_reads_through_with() {
    let out = run_ok(
        r#"
        globalThis.o = { member: 1, fn() {}, u: undefined };
        globalThis.gname = 5;
        console.log((0, eval)("with (o) { typeof member }"), (0, eval)("with (o) { typeof fn }"),
            (0, eval)("with (o) { typeof u }"), (0, eval)("with (o) { typeof nosuchname }"),
            (0, eval)("with (o) { typeof gname }"));
        console.log((0, eval)("with (o) { typeof member === 'number' }"),
            (0, eval)("with (o) { typeof member === typeof gname }"),
            (0, eval)("with (o) { 't:' + typeof member }"),
            (0, eval)("with ({ a: 's' }) { with (o) { typeof a + typeof member } }"));
        var f = Function("with (o) { return typeof member + ':' + typeof gname + ':' + typeof zz; }");
        console.log(f());
        console.log((0, eval)("var q = { member: 1, [Symbol.unscopables]: { member: true } }; globalThis.member = 'g'; with (q) { typeof member }"));
        console.log((0, eval)("var oo = { member: 1 }; var r = []; with (oo) { for (var i = 0; i < 3000; i++) { if (i === 1500) delete oo.member; r.push(typeof member); } } r[0] + r[1499] + r[1500] + r[2999]"));
        console.log((0, eval)("with (o) { (() => typeof member)() + (function () { return typeof member; })() }"));
        "#,
    );
    assert_eq!(
        out,
        [
            "number function undefined undefined number",
            "true true t:number stringnumber",
            "number:number:undefined",
            "string",
            "numbernumberstringstring",
            "numbernumber",
        ]
    );
}

/// #43: `console.log(...)` and its siblings call whatever the program
/// installed, read before the arguments, and return its result.
#[test]
fn replaced_console_methods_are_called() {
    let out = run_ok(
        r#"
        var log = [];
        var orig = { log: console.log, info: console.info, warn: console.warn, error: console.error, debug: console.debug };
        for (var k of ["log", "info", "warn", "error", "debug"]) {
          (function (k) { console[k] = function () { log.push(k + "(" + Array.prototype.join.call(arguments, ",") + ")" + (this === console)); return k + "!"; }; })(k);
        }
        var r = [console.log(1, 2), console.info("i"), console.warn("w"), console.error("e"), console.debug("d")];
        for (var k in orig) console[k] = orig[k];
        console.log(log.join(" "));
        console.log(r.join(","));
        console.warn = function () { throw new Error("boom"); };
        try { console.warn("x"); } catch (e) { console.log("caught", e.message); }
        console.warn = orig.warn;
        var order = [];
        var saved = globalThis.console;
        globalThis.console = { get log() { order.push("get"); return function (a) { order.push("call " + a); }; } };
        console.log((order.push("arg"), "v"));
        globalThis.console = saved;
        console.log(order.join(","));
        var n = 0; console.debug = function () { n++; };
        function f(x) { console.debug("f", x); }
        for (var i = 0; i < 3000; i++) f(i);
        console.debug = orig.debug;
        console.log(n, typeof console.log("v"));
        "#,
    );
    assert_eq!(
        out,
        [
            "log(1,2)true info(i)true warn(w)true error(e)true debug(d)true",
            "log!,info!,warn!,error!,debug!",
            "caught boom",
            "get,arg,call v",
            "v",
            "3000 undefined",
        ]
    );
}

/// #44: `performance` is a global object whose `now` can be read and
/// replaced, by a program or an embedder, with a read-only `timeOrigin`.
#[test]
fn performance_is_a_real_object() {
    let out = run_ok(
        r#"
        console.log(typeof performance, typeof performance.now, performance.now() >= 0, performance.now.name);
        var t0 = performance.now(); var t1 = performance.now();
        console.log(t1 >= t0, "performance" in globalThis);
        var orig = performance.now;
        performance.now = () => 42;
        var s = 0;
        for (var i = 0; i < 3000; i++) s += performance.now();
        console.log(performance.now(), s);
        performance.now = orig;
        console.log(performance.now() !== 42);
        var saved = globalThis.performance;
        globalThis.performance = { now() { return 7; } };
        console.log(performance.now());
        globalThis.performance = saved;
        var order = []; performance.now((order.push("arg"), 1)); console.log(order.join());
        var origin = performance.timeOrigin;
        console.log(typeof origin, origin > 1.7e12, Math.abs(origin + performance.now() - Date.now()) < 50,
            "timeOrigin" in performance);
        performance.timeOrigin = 5;
        console.log(performance.timeOrigin === origin);
        (function () { "use strict"; try { performance.timeOrigin = 5; console.log("no"); } catch (e) { console.log(e.constructor.name); } })();
        "#,
    );
    assert_eq!(
        out,
        [
            "object function true now",
            "true true",
            "42 126000",
            "true",
            "7",
            "arg",
            "number true true true",
            "true",
            "TypeError",
        ]
    );
}

/// #45: a derived class creates no object, so `newTarget.prototype` is
/// read by the base that does, when `super()` reaches it, and not at all
/// by a Proxy base whose `construct` trap makes the object.
#[test]
fn derived_construction_leaves_new_target_prototype_to_the_base() {
    let out = run_ok(
        r#"
        function counter(target, log) {
          return new Proxy(target, { get(t, k, r) { if (k === "prototype") log.push("get"); return Reflect.get(t, k, r); } });
        }
        var log = [];
        const Base = new Proxy(function () {}, { construct() { return {}; } });
        class D0 extends Base {}
        Reflect.construct(D0, [], counter(D0, log));
        console.log(log.length);
        log = [];
        class A { constructor() { log.push("A"); this.a = 1; } }
        class B extends A { constructor() { log.push("B"); super(); log.push("B2"); } }
        class C extends B {}
        var o = Reflect.construct(C, [], counter(C, log));
        console.log(log.join(","), o.a, Object.getPrototypeOf(o) === C.prototype);
        function NT() {}
        NT.prototype = { m() { return "NT"; } };
        class E extends A {}
        console.log(Reflect.construct(E, [], NT).m());
        NT.prototype = 3;
        console.log(Object.getPrototypeOf(Reflect.construct(E, [], NT)) === Object.prototype);
        log = [];
        class M extends Map { constructor(a) { log.push("M"); super(a); } }
        var mm = Reflect.construct(M, [[[1, 2]]], counter(M, log));
        console.log(log.join(","), mm.size, mm instanceof M);
        log = [];
        function F() { log.push("F"); this.f = 1; }
        class G extends F {}
        var go = Reflect.construct(G, [], counter(G, log));
        console.log(log.join(","), go.f);
        log = [];
        class H extends A { constructor() { return { own: 1 }; } }
        console.log(Reflect.construct(H, [], counter(H, log)).own, log.length);
        log = [];
        class J extends A { constructor() { log.push("J"); try { super(); } catch (e) { log.push("caught " + e.message); throw e; } } }
        var bad = new Proxy(J, { get(t, k, r) { if (k === "prototype") throw new Error("nope"); return Reflect.get(t, k, r); } });
        try { Reflect.construct(J, [], bad); } catch (e) { log.push("outer " + e.message); }
        console.log(log.join(","));
        var sum = 0;
        function NT2() {} NT2.prototype = E.prototype;
        for (var i = 0; i < 3000; i++) sum += Reflect.construct(E, [], NT2).a;
        console.log(sum);
        "#,
    );
    assert_eq!(
        out,
        [
            "0",
            "B,get,A,B2 1 true",
            "NT",
            "true",
            "M,get 1 true",
            "get,F 1",
            "1 0",
            "J,caught nope,outer nope",
            "3000",
        ]
    );
}

/// #46 and #47: a write to one of the global object's own bindings never
/// reaches its prototype chain, and a sloppy write to an unresolvable name
/// is [[Set]] on the global object, which does.
#[test]
fn global_object_writes_and_its_prototype_chain() {
    let out = run_ok(
        r#"
        var trapped = [];
        (0, eval)("var declared = 1; function declaredFn() { return 1; }");
        var saved = Object.getPrototypeOf(globalThis);
        Object.setPrototypeOf(globalThis, new Proxy({}, {
          has(t, k) { return k in t; },
          set(t, k, v, r) { trapped.push(String(k)); return true; },
        }));
        console.log(Reflect.set(globalThis, "Math", Math));
        globalThis.JSON = JSON;
        globalThis.declared = 2;
        globalThis.declaredFn = function () { return 2; };
        Reflect.set(globalThis, "Array", Array);
        for (var i = 0; i < 3000; i++) globalThis.parseInt = parseInt;
        console.log(trapped.join(",") || "(none)", declared, declaredFn(), typeof JSON.parse);
        globalThis.fresh1 = 1;
        Reflect.set(globalThis, "fresh2", 2);
        (0, eval)("brandNewGlobal = 1");
        (0, eval)("(function () { inFn = 3; })()");
        console.log(trapped.join(","), Object.prototype.hasOwnProperty.call(globalThis, "brandNewGlobal"));
        Object.setPrototypeOf(globalThis, new Proxy({}, { set(t, k, v, r) { trapped.push("fwd:" + String(k)); return Reflect.set(t, k, v, r); } }));
        (0, eval)("forwarded = 42");
        console.log(trapped.join(","), forwarded, Object.prototype.hasOwnProperty.call(globalThis, "forwarded"));
        Object.setPrototypeOf(globalThis, saved);
        (0, eval)("plainNew = 5");
        console.log(plainNew, Object.prototype.hasOwnProperty.call(globalThis, "plainNew"));
        "#,
    );
    assert_eq!(
        out,
        [
            "true",
            "(none) 2 2 function",
            "fresh1,fresh2,brandNewGlobal,inFn false",
            "fresh1,fresh2,brandNewGlobal,inFn,fwd:forwarded 42 true",
            "5 true",
        ]
    );
}

/// #48: a promise's resolving functions and a combinator's element
/// functions inherit from Function.prototype and keep own properties.
#[test]
fn promise_resolving_functions_are_function_objects() {
    let out = run_ok(
        r#"
        var r1, j1;
        var p1 = new Promise(function (res, rej) { r1 = res; j1 = rej; });
        console.log(typeof r1.call, typeof r1.apply, typeof r1.bind, typeof j1.call, r1.length, JSON.stringify(r1.name));
        console.log(Object.getPrototypeOf(r1) === Function.prototype, r1 instanceof Function, Reflect.ownKeys(r1).join("|"));
        console.log(Function.prototype.toString.call(r1), String(j1));
        r1.extra = 1;
        console.log(r1.extra, Object.keys(r1).join(), "extra" in r1, "call" in r1, delete r1.extra, r1.extra);
        console.log(JSON.stringify({ f: r1, g: 1 }));
        r1.apply(undefined, [5]);
        var r3;
        var p3 = new Promise(function (res) { r3 = res; });
        r3.bind(null, "bound")();
        var allEl;
        Promise.all([{ then: function (a) { allEl = a; } }]);
        var p4 = new Promise(function (res) { res.call(null, "called"); });
        Promise.all([p1, p3, p4]).then(function (v) {
          console.log(v.join(), typeof allEl.call, Function.prototype.toString.call(allEl));
        });
        "#,
    );
    assert_eq!(
        out,
        [
            "function function function function 1 \"\"",
            "true true length|name",
            "function () { [native code] } function () { [native code] }",
            "1 extra true true true undefined",
            "{\"g\":1}",
            "5,bound,called function function () { [native code] }",
        ]
    );
}

/// #49: WeakMap and WeakSet delete in place, and stay coherent with their
/// index across deletes, re-adds and collections.
#[test]
fn weak_collections_delete_in_place() {
    let out = run_ok(
        r#"
        function check(kind) {
          var W = kind === "map" ? new WeakMap() : new WeakSet();
          var M = new Map();
          var keys = []; for (var i = 0; i < 300; i++) keys.push({ i: i });
          var seed = 12345; var rnd = function () { return (seed = (seed * 1103515245 + 12345) & 0x7fffffff) % 300; };
          var bad = 0;
          for (var step = 0; step < 20000; step++) {
            var k = keys[rnd()]; var op = rnd() % 4;
            if (op === 0) { if (kind === "map") { W.set(k, step); } else { W.add(k); } M.set(k, step); }
            else if (op === 1) { if (W.delete(k) !== M.delete(k)) bad++; }
            else if (op === 2) { if (W.has(k) !== M.has(k)) bad++; }
            else if (kind === "map" && W.get(k) !== M.get(k)) bad++;
          }
          for (var j = 0; j < keys.length; j++) { var kk = keys[j]; if (W.has(kk) !== M.has(kk)) bad++; if (kind === "map" && W.get(kk) !== M.get(kk)) bad++; }
          return bad;
        }
        console.log(check("map"), check("set"));
        var wm = new WeakMap(); var ks = []; for (var i = 0; i < 500; i++) { var k = {}; ks.push(k); wm.set(k, i); }
        var ok = true; for (var a = 0; a < 500; a++) ok = wm.delete(ks[a]) && ok; for (var b = 0; b < 500; b++) ok = !wm.has(ks[b]) && ok;
        for (var c = 0; c < 500; c++) wm.set(ks[c], -c); for (var d = 0; d < 500; d++) ok = wm.get(ks[d]) === -d && ok;
        var sym = Symbol("s"); wm.set(sym, "sym"); ok = wm.get(sym) === "sym" && wm.delete(sym) && !wm.has(sym) && ok;
        console.log(ok, wm.delete({}), wm.delete(ks[0]), wm.has(ks[0]));
        var ws = new WeakSet(); for (var e = 0; e < 500; e++) ws.add(ks[e]); for (var f = 0; f < 400; f++) ws.delete(ks[f]);
        console.log(ws.has(ks[0]), ws.has(ks[450]), ws.add(ks[0]) === ws, ws.has(ks[0]));
        var big = new WeakMap(); var live = [];
        for (var g = 0; g < 20000; g++) { var o = {}; big.set(o, g); if (g % 10 === 0) live.push([o, g]); }
        var filler = []; for (var h = 0; h < 200000; h++) filler.push({ h: h });
        var good = 0; for (var m = 0; m < live.length; m++) if (big.get(live[m][0]) === live[m][1]) good++;
        for (var n = 0; n < 100; n++) big.delete(live[n][0]);
        var gone = 0; for (var p = 0; p < 100; p++) if (!big.has(live[p][0])) gone++;
        console.log(good === live.length, gone, filler.length);
        "#,
    );
    assert_eq!(
        out,
        [
            "0 0",
            "true false true false",
            "false true true true",
            "true 100 200000",
        ]
    );
}

/// #50: a global read through the global object finds its name in one
/// lookup however many globals earlier scripts introduced, and reads right.
/// (800 eval globals: the hardened profile's pool holds 1,024.)
#[test]
fn globals_named_through_the_eval_slot_map() {
    let out = run_ok(
        r#"
        globalThis.d = { x: 1 };
        var read = function () { return (0, eval)("(function(){ let s = 0; for (let i = 0; i < 3000; i++) s += d.x; return s; })()"); };
        var before = read();
        for (var i = 0; i < 800; i++) (0, eval)("var g" + i + " = " + i + "; g" + i + ";");
        console.log(before, read());
        console.log((0, eval)("g0 + g799"), typeof g400, (0, eval)("typeof neverDeclared"));
        globalThis.late = 7; console.log((0, eval)("late"));
        (0, eval)("var shadow = 1"); globalThis.shadow = 2; console.log((0, eval)("shadow"));
        Object.defineProperty(globalThis, "acc", { get() { return "getter"; }, configurable: true });
        console.log((0, eval)("acc"));
        delete globalThis.d; try { (0, eval)("d"); console.log("no throw"); } catch (e) { console.log(e.constructor.name); }
        (0, eval)("function evalFn() { return 'fn'; }"); console.log(evalFn(), Object.getOwnPropertyDescriptor(globalThis, "evalFn") !== undefined);
        "#,
    );
    assert_eq!(
        out,
        [
            "3000 3000",
            "799 number undefined",
            "7",
            "2",
            "getter",
            "ReferenceError",
            "fn true",
        ]
    );
}
