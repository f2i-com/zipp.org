//! Global bindings that scripts (indirect eval, `new Function`, an
//! embedder's `eval_in_context`) declare, publish on the global object or
//! read: reflection, deletion, read-only and accessor globals, a frozen
//! global object, and compiled script loops. Every expected line is what
//! Node 24 prints for the same source.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// A script's (indirect eval's) `var` and function declarations are
/// {writable, enumerable, configurable} properties of the global object:
/// reflected, listed, deleted and redeclared as node does, from main code
/// and from compiled script loops.
#[test]
fn script_declarations_are_configurable_global_properties() {
    let out = run_ok(
        r#"
        const E = (s) => (0, eval)(s);
        const desc = (k) => { const d = Object.getOwnPropertyDescriptor(globalThis, k); return d ? JSON.stringify({ v: "value" in d ? (typeof d.value === "function" ? "fn" : d.value) : "acc", w: d.writable, e: d.enumerable, c: d.configurable }) : "none"; };
        const ours = (arr, re) => arr.filter((k) => typeof k === "string" && re.test(k)).sort().join(",");
        const hot = (src) => E("(function(){ let r = []; for (let i = 0; i < 3000; i++) { try { r.push(" + src + "); } catch (e) { r.push(e.constructor.name); } } return [...new Set(r)].join('|'); })")();

        E("var s_v = 1; function s_f() { return 'f'; }");
        console.log(desc("s_v"), desc("s_f"), globalThis.hasOwnProperty("s_v"), "s_f" in globalThis);
        console.log(ours(Object.keys(globalThis), /^s_/), ours(Object.getOwnPropertyNames(globalThis), /^s_/), ours(Reflect.ownKeys(globalThis), /^s_/));
        const fi = []; for (const k in globalThis) if (/^s_/.test(k)) fi.push(k); console.log(fi.sort().join(","));
        console.log(hot("s_v"), hot("s_f()"), E("typeof s_v"), globalThis.s_v, s_f());
        E("s_v = 2"); globalThis.s_v = 3; console.log(E("s_v"), hot("s_v"), desc("s_v"));
        E("(function(){ for (let i = 0; i < 3000; i++) s_v = i; })()"); console.log(globalThis.s_v);
        E("var s_v"); console.log(E("s_v"));
        E("function s_v() { return 'fn'; }"); console.log(typeof globalThis.s_v, desc("s_v"), hot("typeof s_v"));
        E("var s_d1 = 1, s_d2 = 2; function s_d3() {}");
        console.log(E("delete s_d1"), delete globalThis.s_d2, E("delete s_d3"), E("typeof s_d1"), E("typeof s_d2"), desc("s_d3"));
        try { E("s_d1"); console.log("no throw"); } catch (e) { console.log(e.constructor.name); }
        E("var s_d1 = 'again'"); E("var s_d2 = 'again'"); console.log(E("s_d1"), E("s_d2"), desc("s_d2"));
        E("var s_hot = 1");
        console.log(E("(function(){ let s = 0, errs = 0; for (let i = 0; i < 4000; i++) { try { s += s_hot; } catch (e) { errs++; } if (i === 2000) delete globalThis.s_hot; } return s + ':' + errs; })()"));
        "#,
    );
    assert_eq!(
        out,
        [
            "{\"v\":1,\"w\":true,\"e\":true,\"c\":true} {\"v\":\"fn\",\"w\":true,\"e\":true,\"c\":true} true true",
            "s_f,s_v s_f,s_v s_f,s_v",
            "s_f,s_v",
            "1 f number 1 f",
            "3 3 {\"v\":3,\"w\":true,\"e\":true,\"c\":true}",
            "2999",
            "2999",
            "function {\"v\":\"fn\",\"w\":true,\"e\":true,\"c\":true} function",
            "true true true undefined undefined none",
            "ReferenceError",
            "again again {\"v\":\"again\",\"w\":true,\"e\":true,\"c\":true}",
            "2001:1999",
        ]
    );
}

/// `globalThis.x = v` before or after a script names `x`, `this.x = v` in a
/// script, and Reflect.set on the global object: the binding scripts read
/// and write, with the attributes node reports.
#[test]
fn values_published_on_the_global_object_are_bindings() {
    let out = run_ok(
        r#"
        const E = (s) => (0, eval)(s);
        const desc = (k) => { const d = Object.getOwnPropertyDescriptor(globalThis, k); return d ? JSON.stringify({ v: "value" in d ? (typeof d.value === "function" ? "fn" : d.value) : "acc", w: d.writable, e: d.enumerable, c: d.configurable }) : "none"; };
        const ours = (arr, re) => arr.filter((k) => typeof k === "string" && re.test(k)).sort().join(",");
        const hot = (src) => E("(function(){ let r = []; for (let i = 0; i < 3000; i++) { try { r.push(" + src + "); } catch (e) { r.push(e.constructor.name); } } return [...new Set(r)].join('|'); })")();

        globalThis.p_pub = { n: 1 };
        console.log(hot("p_pub.n"), desc("p_pub"));
        E("p_pub = { n: 2 }"); console.log(globalThis.p_pub.n, hot("p_pub.n"));
        globalThis.p_pub = { n: 3 }; console.log(hot("p_pub.n"), E("p_pub.n"));
        console.log(E("delete p_pub"), E("typeof p_pub"), desc("p_pub"));
        globalThis.p_pub = 4; console.log(hot("p_pub"), desc("p_pub"));
        const readLate = E("(function(){ return typeof p_late === 'undefined' ? 'undef' : p_late; })");
        console.log(readLate());
        globalThis.p_late = "late"; console.log(readLate(), desc("p_late"), hot("p_late"));
        E("this.p_this = 'via this'"); console.log(E("p_this"), desc("p_this"));
        E("var p_v = 1; function p_f() { return 'f'; }");
        globalThis.p_v = 2; globalThis.p_f = 3;
        console.log(hot("p_v"), hot("p_f"), desc("p_v"), desc("p_f"));
        Reflect.set(globalThis, "p_v", 5); console.log(hot("p_v"), desc("p_v"));
        console.log(Reflect.set(globalThis, "p_v", 6, {}), hot("p_v"));
        E("p_impl1 = 1"); globalThis.p_impl2 = 2;
        console.log(desc("p_impl1"), desc("p_impl2"), ours(Object.keys(globalThis), /^p_impl/), globalThis.propertyIsEnumerable("p_impl1"));
        const traps = [];
        Object.setPrototypeOf(globalThis, new Proxy(Object.create(Object.getPrototypeOf(globalThis)), {
          set(t, k, v, r) { traps.push(String(k)); return Reflect.set(t, k, v, r); },
        }));
        E("var p_p = 1; p_p = 2; function p_pf() {}"); E("p_pf = 3"); globalThis.p_p = 4; globalThis.p_v = 7;
        console.log(traps.join(","), E("p_p"), E("p_pf"), hot("p_v"));
        "#,
    );
    assert_eq!(
        out,
        [
            "1 {\"v\":{\"n\":1},\"w\":true,\"e\":true,\"c\":true}",
            "2 2",
            "3 3",
            "true undefined none",
            "4 {\"v\":4,\"w\":true,\"e\":true,\"c\":true}",
            "undef",
            "late {\"v\":\"late\",\"w\":true,\"e\":true,\"c\":true} late",
            "via this {\"v\":\"via this\",\"w\":true,\"e\":true,\"c\":true}",
            "2 3 {\"v\":2,\"w\":true,\"e\":true,\"c\":true} {\"v\":3,\"w\":true,\"e\":true,\"c\":true}",
            "5 {\"v\":5,\"w\":true,\"e\":true,\"c\":true}",
            "true 5",
            "{\"v\":1,\"w\":true,\"e\":true,\"c\":true} {\"v\":2,\"w\":true,\"e\":true,\"c\":true} p_impl1,p_impl2 true",
            " 4 3 7",
        ]
    );
}

/// A global made read-only, or an accessor on the global object, governs
/// every write: sloppy writes are ignored or run the setter, strict ones
/// throw, in the interpreter and in compiled script loops.
#[test]
fn read_only_and_accessor_globals_govern_writes() {
    let out = run_ok(
        r#"
        const E = (s) => (0, eval)(s);
        const desc = (k) => { const d = Object.getOwnPropertyDescriptor(globalThis, k); return d ? JSON.stringify({ v: "value" in d ? (typeof d.value === "function" ? "fn" : d.value) : "acc", w: d.writable, e: d.enumerable, c: d.configurable }) : "none"; };
        const ours = (arr, re) => arr.filter((k) => typeof k === "string" && re.test(k)).sort().join(",");
        const hot = (src) => E("(function(){ let r = []; for (let i = 0; i < 3000; i++) { try { r.push(" + src + "); } catch (e) { r.push(e.constructor.name); } } return [...new Set(r)].join('|'); })")();

        E("var r_ro1 = 1"); globalThis.r_ro2 = 1;
        for (const k of ["r_ro1", "r_ro2"]) Object.defineProperty(globalThis, k, { writable: false });
        for (const k of ["r_ro1", "r_ro2"]) {
          let a, b;
          try { E("'use strict'; " + k + " = 100"); a = "no throw"; } catch (e) { a = e.constructor.name; }
          try { E("'use strict'; " + k + "++"); b = "no throw"; } catch (e) { b = e.constructor.name; }
          E(k + " = 200");
          console.log(k, a, b, E(k), hot(k), E("(function(){ for (let i = 0; i < 3000; i++) " + k + " = i; return " + k + "; })()"));
        }
        let backing = 0;
        Object.defineProperty(globalThis, "r_acc", { get() { return backing; }, set(v) { backing = v * 2; }, configurable: true, enumerable: true });
        E("r_acc = 5"); console.log(backing, hot("r_acc"), desc("r_acc"));
        E("(function(){ for (let i = 0; i < 3000; i++) r_acc = i; })()"); console.log(backing);
        E("var r_late = 1"); console.log(hot("r_late"));
        Object.defineProperty(globalThis, "r_late", { get() { return "now a getter"; }, configurable: true });
        console.log(hot("r_late"), desc("r_late"));
        "#,
    );
    assert_eq!(
        out,
        [
            "r_ro1 TypeError TypeError 1 1 1",
            "r_ro2 TypeError TypeError 1 1 1",
            "10 10 {\"v\":\"acc\",\"e\":true,\"c\":true}",
            "5998",
            "1",
            "now a getter {\"v\":\"acc\",\"e\":true,\"c\":true}",
        ]
    );
}

/// preventExtensions and freeze on the global object reach its slot-backed
/// bindings: no new global can be published, existing ones still write
/// until the freeze, and after it every binding is read-only.
#[test]
fn frozen_and_non_extensible_global_object() {
    let out = run_ok(
        r#"
        const E = (s) => (0, eval)(s);
        const desc = (k) => { const d = Object.getOwnPropertyDescriptor(globalThis, k); return d ? JSON.stringify({ v: "value" in d ? (typeof d.value === "function" ? "fn" : d.value) : "acc", w: d.writable, e: d.enumerable, c: d.configurable }) : "none"; };
        const ours = (arr, re) => arr.filter((k) => typeof k === "string" && re.test(k)).sort().join(",");
        const hot = (src) => E("(function(){ let r = []; for (let i = 0; i < 3000; i++) { try { r.push(" + src + "); } catch (e) { r.push(e.constructor.name); } } return [...new Set(r)].join('|'); })")();

        E("var f_v = 1; function f_fn() {} f_impl = 1");
        Object.preventExtensions(globalThis);
        globalThis.f_after = 1; console.log(desc("f_after"), typeof globalThis.f_after);
        try { E("'use strict'; globalThis.f_after2 = 1"); console.log("no throw"); } catch (e) { console.log(e.constructor.name); }
        try { E("var f_new = 1"); console.log("declared"); } catch (e) { console.log(e.constructor.name); }
        globalThis.f_v = 9; E("f_impl = 10"); console.log(E("f_v"), E("f_impl"), hot("f_v"));
        Object.freeze(globalThis);
        try { E("function f_fn() {}"); console.log("declared"); } catch (e) { console.log(e.constructor.name); }
        E("f_v = 'x'; f_impl = 'x'"); globalThis.f_impl = "y";
        console.log(E("f_v"), E("f_impl"), desc("f_v"), desc("f_impl"), desc("f_fn"), desc("Math").slice(-30));
        try { E("'use strict'; f_v = 1"); console.log("no throw"); } catch (e) { console.log(e.constructor.name); }
        console.log(Object.isExtensible(globalThis), hot("f_v"), E("(function(){ for (let i = 0; i < 3000; i++) f_v = i; return f_v; })()"));
        "#,
    );
    assert_eq!(
        out,
        [
            "none undefined",
            "TypeError",
            "TypeError",
            "9 10 9",
            "TypeError",
            "9 10 {\"v\":9,\"w\":false,\"e\":true,\"c\":false} {\"v\":10,\"w\":false,\"e\":true,\"c\":false} {\"v\":\"fn\",\"w\":false,\"e\":true,\"c\":false} \"w\":false,\"e\":false,\"c\":false}",
            "TypeError",
            "false 9 9",
        ]
    );
}

/// Loops in script code compile now; a global object they read replaced
/// mid-run, a global deleted mid-loop and a binding redeclared between runs
/// all read as node reads them.
#[test]
fn compiled_script_loops_follow_replaced_and_deleted_globals() {
    let out = run_ok(
        r#"
        const E = (s) => (0, eval)(s);
        const desc = (k) => { const d = Object.getOwnPropertyDescriptor(globalThis, k); return d ? JSON.stringify({ v: "value" in d ? (typeof d.value === "function" ? "fn" : d.value) : "acc", w: d.writable, e: d.enumerable, c: d.configurable }) : "none"; };
        const ours = (arr, re) => arr.filter((k) => typeof k === "string" && re.test(k)).sort().join(",");
        const hot = (src) => E("(function(){ let r = []; for (let i = 0; i < 3000; i++) { try { r.push(" + src + "); } catch (e) { r.push(e.constructor.name); } } return [...new Set(r)].join('|'); })")();

        E("var c_obj = { x: 1 }");
        const sum = E("(function(){ let r = 0; for (let i = 0; i < 100000; i++) r += c_obj.x; return r; })");
        console.log(sum());
        E("c_obj = { x: 2 }"); console.log(sum());
        globalThis.c_obj = { y: 1, x: 3 }; console.log(sum());
        const mixed = E("(function(){ let r = 0; for (let i = 0; i < 100000; i++) { r += c_obj.x; if (i === 50000) c_obj = { x: 10 }; } return r; })");
        console.log(mixed());
        globalThis.c_cnt = 0;
        console.log(E("(function(){ for (let i = 0; i < 100000; i++) c_cnt++; return c_cnt; })()"), globalThis.c_cnt);
        const viaFn = new Function("let r = 0; for (let i = 0; i < 100000; i++) r = (r + i * 3) | 0; return r;");
        console.log(viaFn(), viaFn());
        E("function c_add(a, b) { return a + b; }");
        console.log(E("(function(){ let r = 0; for (let i = 0; i < 100000; i++) r = c_add(r, i); return r; })()"));
        E("function c_add(a, b) { return a - b; }");
        console.log(E("(function(){ let r = 0; for (let i = 0; i < 100000; i++) r = c_add(r, i); return r; })()"));
        "#,
    );
    assert_eq!(
        out,
        [
            "100000",
            "200000",
            "300000",
            "649993",
            "100000 100000",
            "2114948112 2114948112",
            "4999950000",
            "-4999950000",
        ]
    );
}
