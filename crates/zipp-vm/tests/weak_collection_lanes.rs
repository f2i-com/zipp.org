//! WeakMap and WeakSet calls from compiled code: the fused `m.get(k)` lanes
//! and the captured-callee lane serve a weak collection through its own
//! intrinsic proof. Every expected line is what Node 24 prints.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// WeakMap and WeakSet calls from hot compiled loops and compiled
/// functions (the fused and the captured lanes): the intrinsic, a subclass
/// override, an own method, a prototype method swapped mid-loop, invalid
/// and symbol keys, and the brand checks.
#[test]
fn weak_collection_calls_from_compiled_code() {
    let out = run_ok(
        r#"
        const keys = []; for (let i = 0; i < 64; i++) keys.push({ i });
        const wm = new WeakMap(); const ws = new WeakSet();
        for (const k of keys) { wm.set(k, k.i); if (k.i % 2) ws.add(k); }
        function loopGet(m) { let r = 0; for (let i = 0; i < 20000; i++) { const v = m.get(keys[i & 63]); r += v === undefined ? -1 : v; } return r; }
        function loopHas(m) { let r = 0; for (let i = 0; i < 20000; i++) if (m.has(keys[i & 63])) r++; return r; }
        function loopSetDel(m) { let r = 0; for (let i = 0; i < 20000; i++) { const k = keys[i & 63]; if (i & 1) m.set(k, i); else r += m.delete(k) ? 1 : 0; } return r; }
        function loopAddDel(s) { let r = 0; for (let i = 0; i < 20000; i++) { const k = keys[i & 63]; if (i & 1) s.add(k); else r += s.delete(k) ? 1 : 0; } return r; }
        function viaFn(m, k) { return m.get(k); }
        console.log(loopGet(wm), loopHas(wm), loopHas(ws));
        console.log(loopSetDel(wm), loopGet(wm), loopAddDel(ws), loopHas(ws));
        console.log((() => { let r = 0; for (let i = 0; i < 20000; i++) r += viaFn(wm, keys[i & 63]) | 0; return r; })());
        console.log((() => { let r = 0; for (let i = 0; i < 3000; i++) { try { wm.set(i % 7 ? keys[0] : i, 1); } catch (e) { r++; } } return r; })(), wm.get(1), wm.has("x"), wm.delete(null), ws.has(2));
        const sym = Symbol("s");
        console.log(wm.set(sym, "sym").get(sym), wm.has(sym), (() => { try { wm.set(Symbol.for("reg"), 1); return "no throw"; } catch (e) { return e.constructor.name; } })());
        class SubWM extends WeakMap { get(k) { return "sub:" + super.get(k); } }
        const sw = new SubWM(); sw.set(keys[0], 5);
        console.log((() => { let r = ""; for (let i = 0; i < 3000; i++) r = sw.get(keys[0]); return r; })(), (() => { let r = ""; for (let i = 0; i < 3000; i++) r = viaFn(sw, keys[0]); return r; })());
        const own = new WeakMap(); own.set(keys[1], 7); own.get = function (k) { return "own"; };
        console.log((() => { let r = ""; for (let i = 0; i < 3000; i++) r = own.get(keys[1]); return r; })(), (() => { let r = ""; for (let i = 0; i < 3000; i++) r = viaFn(own, keys[1]); return r; })());
        const origGet = WeakMap.prototype.get;
        console.log((() => { let r = []; for (let i = 0; i < 6000; i++) { if (i === 2000) WeakMap.prototype.get = function () { return "patched"; }; if (i === 4000) WeakMap.prototype.get = origGet; r.push(viaFn(wm, keys[3])); } return [...new Set(r)].join("|"); })());
        const g = WeakMap.prototype.get;
        console.log((() => { let r = 0; for (let i = 0; i < 3000; i++) r += g.call(wm, keys[2]); return r; })());
        console.log([() => g.call(new Map(), keys[2]), () => g.call(ws, keys[2]), () => Map.prototype.get.call(wm, keys[2])].map((f) => { try { f(); return "no throw"; } catch (e) { return e.constructor.name; } }).join(","));
        "#,
    );
    assert_eq!(
        out,
        [
            "629488 20000 10000",
            "32 199670256 0 10000",
            "199680256",
            "429 undefined false false false",
            "sym true TypeError",
            "sub:5 sub:5",
            "own own",
            "19971|patched",
            "NaN",
            "TypeError,TypeError,TypeError",
        ]
    );
}
