//! GC and heap regressions from the 15 September 2026 review (E-gc-heap).
//!
//! * A module namespace (and a deferred namespace) has its whole `ObjMap`
//!   replaced after the module body ran — by then a minor may have promoted
//!   it, so the fresh `@@toStringTag` string needs a barrier.
//! * `RegExp.lastIndex` holds the assigned Value raw; the collector must
//!   trace it and its stores must be barriered.
//! * `String.prototype.split()` with no separator builds an OLD-born array
//!   holding the (possibly young) receiver.
//! * `new Error(msg, {get cause(){…}})`, `Reflect.construct(Error, …)`, and
//!   `Object.create(proto, {…getter descriptors…})` held their fresh result
//!   in a Rust local while guest code collected.
//! * Loops closed by a backward conditional jump (`do … while`) or resumed
//!   through a `finally`, `setTimeout` callbacks, Promise combinators over a
//!   user iterator, and `using` disposers never reached a collection.
//! * Timer callbacks are tasks: a microtask checkpoint follows each, and an
//!   uncaught throw is the program's error.
//! * A generator abandoned inside a `using` block no longer roots its
//!   resources for the life of the VM; an async stack's `@@dispose` fallback
//!   keeps a getter's fresh method rooted while the shim compiles.
//! * [[Set]] through a function, class, array or other exotic prototype
//!   honours its accessors and non-writable properties.
//! * `export * from` an in-flight module sees that module's pending star
//!   names; long property keys never enter the thread-local shape table.
//! * A global eval's function declaration stored into the global object is
//!   barriered.
//!
//! The collector modes latch per process, so the GC cases run in child
//! processes under the ordinary nursery, nursery verification with stress,
//! majors-only stress, and the interpreter tier.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const CHILD_ENV: &str = "ZIPP_GC_HEAP_AUDIT_CHILD";
const CHURN_ENV: &str = "ZIPP_GC_HEAP_AUDIT_CHURN";
const CASE_ENV: &str = "ZIPP_GC_HEAP_AUDIT_CASE";

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "zipp-gc-heap-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        for (file, src) in files {
            std::fs::write(dir.join(file), src).expect("write fixture");
        }
        Self(dir)
    }

    fn module(&self, entry: &str) -> zipp_vm::Outcome {
        zipp_vm::run_module_file(&self.0.join(entry), None).expect("module compiles")
    }

    fn module_ok(&self, entry: &str) -> String {
        let out = self.module(entry);
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        out.output.join("\n")
    }

    fn script_ok(&self, src: &str) -> String {
        let out = zipp_vm::run_with_base(src, Some(self.0.clone())).expect("script compiles");
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        out.output.join("\n")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn out(source: &str) -> String {
    let result = zipp_vm::run(source).expect("compile gc-heap regression");
    assert!(
        result.error.is_none(),
        "unexpected throw: {:?}\nsource:\n{source}",
        result.error
    );
    result.output.join("\n")
}

/// Allocation churn sized for the mode: enough to cross several young
/// budgets normally, small under collection-per-safe-point stress.
fn churn_count() -> usize {
    std::env::var(CHURN_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(70_000)
}

fn churn_fn(n: usize) -> String {
    format!(
        "function churn() {{ let k = []; for (let i = 0; i < {n}; i++) {{ k.push({{ d: i, s: 's' + i, a: [i] }}); if (k.length > 500) k = []; }} return k.length; }}"
    )
}

#[test]
fn audit_20260915_gc_heap_child() {
    if std::env::var_os(CHILD_ENV).is_none() || std::env::var_os(CASE_ENV).is_some() {
        return;
    }
    let n = churn_count();
    let churn = churn_fn(n);

    // Namespace @@toStringTag after the body promoted the namespace: dynamic
    // import, static module entry, and a deferred namespace triggered late.
    let heavy = format!("{churn}\nchurn();\nexport const obj = {{ payload: 'x' }};\n");
    let fx = Fixture::new(
        "namespace-tag",
        &[
            ("heavy.mjs", &heavy),
            ("lazy.mjs", "export const v = 42;\n"),
            (
                "entry.mjs",
                &format!(
                    "import * as ns from './heavy.mjs';\n{churn}\nchurn();\n\
                     console.log(ns[Symbol.toStringTag], Object.prototype.toString.call(ns), ns.obj.payload);\n"
                ),
            ),
            (
                "deferred.mjs",
                &format!(
                    "import defer * as lz from './lazy.mjs';\n{churn}\nchurn();\nconst v = lz.v;\nchurn();\n\
                     console.log(v, lz[Symbol.toStringTag], Object.prototype.toString.call(lz));\n"
                ),
            ),
        ],
    );
    assert_eq!(fx.module_ok("entry.mjs"), "Module [object Module] x");
    assert_eq!(
        fx.module_ok("deferred.mjs"),
        "42 Deferred Module [object Deferred Module]"
    );
    assert_eq!(
        fx.script_ok(&format!(
            "{churn}\nimport('./heavy.mjs').then(ns => {{ churn(); \
             console.log(ns[Symbol.toStringTag], JSON.stringify(Object.getOwnPropertyDescriptor(ns, Symbol.toStringTag))); }});"
        )),
        "Module {\"value\":\"Module\",\"writable\":false,\"enumerable\":false,\"configurable\":false}"
    );

    // RegExp lastIndex keeps an assigned object alive and old->young stores
    // into an old RegExp are seen by minors.
    assert_eq!(
        out(&format!(
            r#"
            {churn}
            var re = /a/g; churn();
            (function () {{ re.lastIndex = {{ mine: 1 }}; }})();
            churn();
            console.log(typeof re.lastIndex, JSON.stringify(re.lastIndex));
            var sticky = /x/y; churn();
            (function () {{ sticky.lastIndex = {{ valueOf() {{ return 1; }} }}; }})();
            churn();
            console.log(sticky.exec("ax") !== null, sticky.lastIndex);
            var defined = /b/g; churn();
            (function () {{ Object.defineProperty(defined, "lastIndex", {{ value: {{ def: 2 }} }}); }})();
            churn();
            console.log(JSON.stringify(defined.lastIndex));
            var viaString = /c/g; churn();
            (function () {{ viaString.lastIndex = "q" + Math.floor(Math.random() * 0) + "z"; }})();
            churn();
            console.log(viaString.lastIndex);
            "#
        )),
        "object {\"mine\":1}\ntrue 2\n{\"def\":2}\nq0z"
    );

    // split() / split(undefined) put the receiver into a pretenured array.
    assert_eq!(
        out(&format!(
            r#"
            {churn}
            var keep = [];
            for (var i = 0; i < 300; i++) keep.push(("str-" + i + "-" + Math.random()).slice(0).split());
            for (var i = 0; i < 300; i++) keep.push(("k" + i).split(undefined));
            churn(); churn();
            var bad = 0;
            for (var i = 0; i < keep.length; i++) {{
                var s = keep[i][0];
                if (typeof s !== "string" || !(s.startsWith("str-") || s.startsWith("k"))) bad++;
            }}
            console.log("bad", bad, keep.length);
            "#
        )),
        "bad 0 600"
    );

    // Error construction with guest code in InstallErrorCause.
    assert_eq!(
        out(&format!(
            r#"
            {churn}
            const e = new Error("msg", {{ get cause() {{ churn(); return {{ c: "c" }}; }} }});
            churn();
            console.log(typeof e, e instanceof Error, e.message, e.cause.c);
            const r = Reflect.construct(Error, ["x1", {{ get cause() {{ churn(); return "cc"; }} }}]);
            churn();
            console.log(JSON.stringify([r.message, r.cause]), r instanceof Error);
            const T = TypeError;
            const t = new T("tm", new Proxy({{}}, {{ has() {{ churn(); return true; }}, get() {{ return "pc"; }} }}));
            churn();
            console.log(t instanceof TypeError, t.message, t.cause);
            const ag = new AggregateError([1, 2], "am", {{ get cause() {{ churn(); return "ac"; }} }});
            churn();
            console.log(ag instanceof AggregateError, ag.message, ag.cause, ag.errors.join());
            const A = AggregateError;
            const ag2 = Reflect.construct(A, [[3, 4], "am2", {{ get cause() {{ churn(); return {{ deep: "ac2" }}; }} }}]);
            churn();
            console.log(ag2 instanceof AggregateError, ag2.message, ag2.cause.deep, ag2.errors.join());
            const NT = new Proxy(function () {{}}, {{
                get(target, key) {{
                    if (key === "prototype") {{ churn(); return Object.create(RangeError.prototype, {{ tag: {{ value: "nt" }} }}); }}
                    return target[key];
                }}
            }});
            const nt = Reflect.construct(RangeError, ["ntm", {{ get cause() {{ churn(); return "ntc"; }} }}], NT);
            churn();
            console.log(nt instanceof RangeError, nt.tag, nt.message, nt.cause);
            "#
        )),
        "object true msg c\n[\"x1\",\"cc\"] true\ntrue tm pc\ntrue am ac 1,2\ntrue am2 ac2 3,4\ntrue nt ntm ntc"
    );

    // Promise combinators and DisposableStack root their Rust-held values
    // (iterator, capability, per-element promise and resolvers, disposer list,
    // SuppressedError chain) instead of suspending collection.
    assert_eq!(
        out(&r#"
            CHURN
            const log = [];
            function* g() { for (let i = 0; i < 3; i++) { churn(); yield i % 2 ? Promise.resolve({ v: 'p' + i }) : { then(r) { churn(); r({ v: 't' + i }); } }; } }
            Promise.all(g()).then(v => log.push('all ' + v.map(x => x.v).join()));
            Promise.allSettled(g()).then(v => log.push('settled ' + v.map(x => x.status + ':' + x.value.v).join()));
            Promise.any(g()).then(v => log.push('any ' + v.v));
            Promise.race(g()).then(v => log.push('race ' + v.v));
            class P extends Promise {}
            P.all(g()).then(v => log.push('sub ' + v.length + ' ' + (v[2].v)));
            Promise.all({ [Symbol.iterator]() { let i = 0; return { next() { churn(); return i < 2 ? { value: { k: i++ }, done: false } : { done: true }; } }; } }).then(v => log.push('custom ' + v.map(x => x.k).join()));
            Promise.all({ [Symbol.iterator]() { let i = 0; return { next() { churn(); if (i === 1) throw new Error('boom'); return { value: i++, done: false }; } }; } }).catch(e => log.push('threw ' + e.message));
            const ds = new DisposableStack();
            ds.use({ [Symbol.dispose]() { churn(); throw new Error('d1'); } });
            ds.adopt({ tag: 'r2' }, (r) => { churn(); throw new Error('d2:' + r.tag); });
            ds.defer(() => { churn(); });
            try { ds.dispose(); } catch (e) { log.push('dispose ' + (e instanceof SuppressedError) + ' ' + e.error.message + ' ' + e.suppressed.message); }
            const ads = new AsyncDisposableStack();
            const res = { n: 0, get [Symbol.dispose]() { this.n++; churn(); return function () { log.push('async-fallback ' + res.n); }; } };
            ads.use(res);
            churn();
            ads.disposeAsync().then(() => log.push('async-done'), e => log.push('async-rejected ' + e));
            setTimeout(() => console.log(log.join('|')), 0);
            "#
        .replace("CHURN", &churn)),
        "dispose true d1 d2:r2|async-fallback 1|threw boom|any p1|race p1|custom 0,1|async-done|all t0,p1,t2|settled fulfilled:t0,fulfilled:p1,fulfilled:t2|sub 3 t2"
    );

    // Iterator.zip steps hold earlier step values (and a strict-mode
    // TypeError) across the other iterators' guest next()/return() calls.
    assert_eq!(
        out(&r#"
            CHURN
            const log = [];
            function src(tag, n) {
              let i = 0;
              return { [Symbol.iterator]() { return this; }, next() { churn(); return i < n ? { value: { v: tag + (i++) }, done: false } : { value: undefined, done: true }; }, return() { churn(); log.push('ret ' + tag); return {}; } };
            }
            log.push([...Iterator.zip([src('a', 2), src('b', 3)])].map(t => t.map(x => x.v).join('+')).join(','));
            log.push([...Iterator.zip([src('c', 1), src('d', 3)], { mode: 'longest', padding: [{ v: 'P' }, { v: 'Q' }] })].map(t => t.map(x => x.v).join('+')).join(','));
            try { [...Iterator.zip([src('e', 2), src('f', 1), src('g', 5)], { mode: 'strict' })]; } catch (e) { log.push(e.constructor.name); }
            log.push(JSON.stringify([...Iterator.zipKeyed({ x: src('h', 2), y: src('i', 2) })].map(o => o.x.v + o.y.v)));
            console.log(log.join('|'));
            "#
        .replace("CHURN", &churn)),
        "ret b|a0+b0,a1+b1|c0+d0,P+d1,P+d2|ret g|ret e|TypeError|ret i|[\"h0i0\",\"h1i1\"]"
    );

    // [[Set]] through a function, class, array, Map, RegExp or String wrapper
    // prototype honours its accessors and non-writable own properties (also
    // the synthesized `name`/`length`/`prototype`/`caller`), in every tier.
    assert_eq!(
        out(r#"
            function t(name, proto, strictMode) {
              let threw = false;
              const o = Object.create(proto);
              try {
                if (strictMode) (function () { 'use strict'; o.x = 2; })(); else o.x = 2;
              } catch (e) { threw = e.constructor.name; }
              return name + ' ' + JSON.stringify([Object.hasOwn(o, 'x'), threw]);
            }
            let hits = 0;
            const arrS = []; Object.defineProperty(arrS, 'x', { set(v) { hits++; } });
            const arrNW = []; Object.defineProperty(arrNW, 'x', { value: 1, writable: false });
            const fnS = function () {}; Object.defineProperty(fnS, 'x', { set(v) { hits++; } });
            const fnNW = function () {}; Object.defineProperty(fnNW, 'x', { value: 1, writable: false });
            const mapS = new Map(); Object.defineProperty(mapS, 'x', { set(v) { hits++; } });
            const clsS = class { static set x(v) { hits++; } };
            const clsG = class { static get x() { return 1; } };
            const clsD = class { static x = 5; };
            const out = [];
            for (const [n, p] of [['arrS', arrS], ['arrNW', arrNW], ['fnS', fnS], ['fnNW', fnNW], ['mapS', mapS], ['clsS', clsS], ['clsG', clsG], ['clsD', clsD]]) {
              out.push(t(n, p, false)); out.push(t(n + '-strict', p, true));
            }
            const f = function named() {};
            const on = Object.create(f); on.name = 'z'; on.length = 9; on.prototype = 1;
            out.push('fn-intrinsics ' + JSON.stringify([Object.hasOwn(on, 'name'), Object.hasOwn(on, 'length'), Object.hasOwn(on, 'prototype')]));
            const oc = Object.create(class K {}); oc.prototype = 1; oc.name = 'q';
            out.push('class-intrinsics ' + JSON.stringify([Object.hasOwn(oc, 'prototype'), Object.hasOwn(oc, 'name')]));
            const oa = Object.create([1, 2, 3]); oa.length = 7; oa[0] = 9;
            out.push('array ' + JSON.stringify([Object.hasOwn(oa, 'length'), oa.length, Object.hasOwn(oa, '0')]));
            const fa = Object.freeze([1]); const ofa = Object.create(fa); ofa[0] = 5; ofa.length = 3;
            out.push('frozen-array ' + JSON.stringify([Object.hasOwn(ofa, '0'), Object.hasOwn(ofa, 'length')]));
            const re = /x/g; const ore = Object.create(re); ore.lastIndex = 4;
            out.push('regexp ' + JSON.stringify([Object.hasOwn(ore, 'lastIndex'), re.lastIndex]));
            const bs = new String('ab'); const obs = Object.create(bs); obs[0] = 'z'; obs.length = 1; obs[5] = 'q';
            out.push('string ' + JSON.stringify([Object.hasOwn(obs, '0'), Object.hasOwn(obs, 'length'), Object.hasOwn(obs, '5')]));
            const g = function () {}; const og = Object.create(g); og.caller = 1; og.arguments = 2;
            out.push('legacy ' + JSON.stringify([Object.hasOwn(og, 'caller'), Object.hasOwn(og, 'arguments')]));
            function plain() {} plain.z = 1; const fp = Object.create(plain); fp.z = 3;
            out.push('fn-data ' + JSON.stringify([Object.hasOwn(fp, 'z'), fp.z, plain.z]));
            function put(o, v) { o.x = v; return o; }
            let own = 0;
            for (let i = 0; i < 3000; i++) for (const p of [fnS, clsS, arrS]) if (Object.hasOwn(put(Object.create(p), i), 'x')) own++;
            out.push('hits ' + hits + ' own ' + own);
            console.log(out.join('\n'));
        "#),
        "arrS [false,false]\narrS-strict [false,false]\narrNW [false,false]\narrNW-strict [false,\"TypeError\"]\n\
         fnS [false,false]\nfnS-strict [false,false]\nfnNW [false,false]\nfnNW-strict [false,\"TypeError\"]\n\
         mapS [false,false]\nmapS-strict [false,false]\nclsS [false,false]\nclsS-strict [false,false]\n\
         clsG [false,false]\nclsG-strict [false,\"TypeError\"]\nclsD [true,false]\nclsD-strict [true,false]\n\
         fn-intrinsics [false,false,true]\nclass-intrinsics [false,false]\narray [true,7,true]\n\
         frozen-array [false,false]\nregexp [true,0]\nstring [false,false,true]\nlegacy [false,false]\n\
         fn-data [true,3,1]\nhits 9008 own 0"
    );

    // Object.create / defineProperty descriptors whose getters collect.
    assert_eq!(
        out(&format!(
            r#"
            {churn}
            const create = Object.create;
            let bad = 0;
            for (let i = 0; i < 4; i++) {{
                const o = Object.create(null, {{ a: {{ enumerable: true, get value() {{ churn(); return i; }} }} }});
                if (typeof o !== "object" || o.a !== i) bad++;
                const p = create({{ base: 1 }}, {{ a: {{ enumerable: true, get value() {{ churn(); return {{ i }}; }} }}, b: {{ get value() {{ return "B"; }} }} }});
                if (typeof p !== "object" || p.a.i !== i || p.b !== "B" || p.base !== 1) bad++;
            }}
            const d = {{}};
            Object.defineProperty(d, "x", {{ get value() {{ return {{ fresh: "v" }}; }}, get writable() {{ churn(); return true; }} }});
            churn();
            const props = {{ y: {{ get value() {{ delete props.y; churn(); return "gone"; }}, get writable() {{ churn(); return false; }} }} }};
            const q = Object.defineProperties({{}}, props);
            churn();
            console.log("bad", bad, d.x.fresh, q.y);
            "#
        )),
        "bad 0 v gone"
    );

    // A global sloppy eval defines its function declarations on the (old)
    // global object; a minor used to free them ("… is not a function").
    assert_eq!(
        out(&format!(
            r#"
            {churn}
            churn();
            eval('function evalDeclared() {{ return "d"; }}');
            (0, eval)('function* evalGen() {{ yield "g"; }}');
            churn();
            console.log(evalDeclared(), evalGen().next().value, typeof globalThis.evalGen,
                Object.getOwnPropertyDescriptor(globalThis, "evalGen").configurable);
            "#
        )),
        "d g function true"
    );
}

/// Each collection-reach case runs alone in its own process: the peak-slot
/// figure is a process-wide high-water mark.
#[test]
fn audit_20260915_gc_heap_collects_child() {
    let Ok(case) = std::env::var(CASE_ENV) else {
        return;
    };
    let src = match case.as_str() {
        "do-while" => {
            "let i = 0, a; do { a = { x: i, y: [i, i, i, i] }; i++; } while (i < 400000); console.log(a.x);"
        }
        "do-while-fn" => {
            "function run(n) { let i = 0, a; do { a = { x: i, y: [i, i, i, i] }; i++; if (i % 2) continue; } while (i < n); return a.x; }\n\
             for (let w = 0; w < 300; w++) run(8);\n\
             console.log(run(400000));"
        }
        "finally-continue" => {
            "let i = 0, a; while (i < 400000) { try { a = { x: i, y: [i, i, i, i] }; i++; continue; } finally {} } console.log(a.x);"
        }
        "timer" => {
            "setTimeout(() => { let a; for (let i = 0; i < 400000; i++) a = { x: i, y: [i, i, i, i] }; console.log(a.x); }, 0);"
        }
        "promise-all-iterator" => {
            "function* gen() { let a; for (let i = 0; i < 400000; i++) a = { x: i, y: [i, i, i, i] }; yield a.x; }\n\
             Promise.all(gen()).then(v => console.log(v[0]));"
        }
        "disposer" => {
            "{ using r = { [Symbol.dispose]() { let a; for (let i = 0; i < 400000; i++) a = { x: i, y: [i, i, i, i] }; console.log(a.x); } }; }"
        }
        "using-abandoned" => {
            // Run under majors-only stress: a generator dropped while suspended
            // inside a `using` block must not keep its resource reachable
            // (finished and returned generators still dispose).
            assert_eq!(
                out(r#"
                    const refs = [], log = [];
                    function* g() { using a = { big: new Array(100).fill(1), [Symbol.dispose]() { log.push('disposed'); } }; refs.push(new WeakRef(a)); yield 1; yield 2; }
                    for (let i = 0; i < 50; i++) { const it = g(); it.next(); }
                    const done = g(); done.next(); done.next(); done.next();
                    const early = g(); early.next(); early.return();
                    setTimeout(() => { const o = [{}, {}]; console.log(refs.filter(r => r.deref() !== undefined).length, log.length); }, 0);
                "#),
                "0 2"
            );
            return;
        }
        other => panic!("unknown case {other}"),
    };
    assert_eq!(out(src), "399999");
    let (minors, majors, _, _, _, _, _, peak_slots) = zipp_vm::gc_nursery_stats();
    assert!(
        minors + majors >= 8 && peak_slots < 400_000,
        "{case}: 800k short-lived allocations collected {minors} minors + {majors} majors, peak {peak_slots} slots"
    );
}

#[test]
fn gc_heap_cases_agree_across_collector_and_tier_modes() {
    if std::env::var_os(CHILD_ENV).is_some() || std::env::var_os(CASE_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("gc-heap audit test binary");
    for (mode, vars) in [
        ("nursery-verified", &[("ZIPP_NURSERY_VERIFY", "1")][..]),
        (
            "nursery-verified-holder-grain",
            &[
                ("ZIPP_NURSERY_VERIFY", "1"),
                ("ZIPP_NO_VALGRAIN_REMSET", "1"),
            ][..],
        ),
        (
            "nursery-verified-interpreter",
            &[("ZIPP_NURSERY_VERIFY", "1"), ("ZIPP_NOJIT", "1")][..],
        ),
        (
            "stress-verified",
            &[
                ("ZIPP_GC_STRESS", "1"),
                ("ZIPP_NURSERY_VERIFY", "1"),
                (CHURN_ENV, "60"),
            ][..],
        ),
        (
            "majors-only-stress",
            &[
                ("ZIPP_GC_STRESS", "1"),
                ("ZIPP_NO_NURSERY", "1"),
                (CHURN_ENV, "40"),
            ][..],
        ),
    ] {
        let output = Command::new(&exe)
            .args(["--exact", "audit_20260915_gc_heap_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env_remove(CASE_ENV)
            .env_remove(CHURN_ENV)
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NURSERY_VERIFY")
            .env_remove("ZIPP_NO_NURSERY")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_VALGRAIN_REMSET")
            .envs(vars.iter().copied())
            .output()
            .expect("spawn gc-heap audit child");
        assert!(
            output.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn loops_and_timers_reach_collections_in_both_tiers() {
    if std::env::var_os(CHILD_ENV).is_some() || std::env::var_os(CASE_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("gc-heap audit test binary");
    for case in [
        "do-while",
        "do-while-fn",
        "finally-continue",
        "timer",
        "promise-all-iterator",
        "disposer",
    ] {
        for (tier, vars) in [
            ("default", &[][..]),
            ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ] {
            let output = Command::new(&exe)
                .args(["--exact", "audit_20260915_gc_heap_collects_child", "--nocapture"])
                .env(CASE_ENV, case)
                .env_remove(CHILD_ENV)
                .env_remove("ZIPP_GC_STRESS")
                .env_remove("ZIPP_NURSERY_VERIFY")
                .env_remove("ZIPP_NO_NURSERY")
                .env_remove("ZIPP_NOJIT")
                .envs(vars.iter().copied())
                .output()
                .expect("spawn gc-heap collection child");
            assert!(
                output.status.success(),
                "{case} ({tier}) failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn abandoned_using_generators_release_their_resources() {
    if std::env::var_os(CHILD_ENV).is_some() || std::env::var_os(CASE_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("gc-heap audit test binary");
    for (tier, vars) in [
        ("default", &[][..]),
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
    ] {
        let output = Command::new(&exe)
            .args(["--exact", "audit_20260915_gc_heap_collects_child", "--nocapture"])
            .env(CASE_ENV, "using-abandoned")
            .env("ZIPP_GC_STRESS", "1")
            .env("ZIPP_NO_NURSERY", "1")
            .env_remove(CHILD_ENV)
            .env_remove("ZIPP_NURSERY_VERIFY")
            .env_remove("ZIPP_NOJIT")
            .envs(vars.iter().copied())
            .output()
            .expect("spawn gc-heap using child");
        assert!(
            output.status.success(),
            "using-abandoned ({tier}) failed:
--- stdout ---
{}
--- stderr ---
{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn timer_callbacks_are_tasks_with_checkpoints_and_uncaught_errors() {
    if std::env::var_os(CHILD_ENV).is_some() || std::env::var_os(CASE_ENV).is_some() {
        return;
    }
    // A microtask queued by a timer runs before the next timer.
    assert_eq!(
        out(r#"
            setTimeout(() => { console.log("timeout1"); Promise.resolve().then(() => console.log("timeout1-micro")); }, 0);
            setTimeout(() => console.log("timeout2"), 0);
            setTimeout(() => { try { throw new Error("inner"); } catch (e) { console.log("caught", e.message); } }, 1);
        "#),
        "timeout1\ntimeout1-micro\ntimeout2\ncaught inner"
    );
    // An uncaught throw ends the loop and is the program's error.
    let result = zipp_vm::run(
        r#"
        console.log("start");
        setTimeout(() => { throw new TypeError("timer boom"); }, 0);
        setTimeout(() => console.log("second timer ran"), 5);
        "#,
    )
    .expect("compiles");
    assert_eq!(result.output, vec!["start"]);
    assert_eq!(result.error.as_deref(), Some("TypeError: timer boom"));
    // A main-script throw stays the reported error.
    let result = zipp_vm::run(
        r#"
        setTimeout(() => { throw new Error("later"); }, 0);
        throw new RangeError("first");
        "#,
    )
    .expect("compiles");
    assert_eq!(result.error.as_deref(), Some("RangeError: first"));
    // A throw with no heap value (stack exhaustion) is still reported.
    let result = zipp_vm::run(
        "setTimeout(function f() { f(); }, 0); setTimeout(() => console.log('after'), 5);",
    )
    .expect("compiles");
    assert!(result.output.is_empty(), "{:?}", result.output);
    assert!(
        result.error.as_deref().is_some_and(|e| e.contains("RangeError")),
        "{:?}",
        result.error
    );
}

/// A star cycle whose second star target imports from a module still
/// linking: linking must never evaluate that target early (it would read an
/// uninitialised binding) and bodies run in post-order. Collecting an
/// in-flight module's pending star names (R064, reverted pending a
/// link/evaluate split, R066) broke this program. The namespace of the
/// module that closes the cycle still misses the late target's names (the
/// open R064 gap), so only the order and the entry namespace are pinned.
#[test]
fn star_cycles_link_without_evaluating_star_targets_early() {
    if std::env::var_os(CHILD_ENV).is_some() || std::env::var_os(CASE_ENV).is_some() {
        return;
    }
    let fx = Fixture::new(
        "star-cycle",
        &[
            ("a.mjs", "export * from './b.mjs'; export * from './c.mjs'; export const x = 1;\n"),
            ("b.mjs", "export * from './a.mjs'; export const y = 2; console.log('b');\n"),
            ("c.mjs", "import { y } from './b.mjs'; console.log('c', y); export const z = 3;\n"),
            ("entry.mjs", "import * as na from './a.mjs'; console.log(Object.keys(na).join());\n"),
            ("a2.mjs", "export * from './b2.mjs'; export * from './c2.mjs'; export const x = 1; console.log('a');\n"),
            ("b2.mjs", "export * from './a2.mjs'; export const y = 2; console.log('b');\n"),
            ("c2.mjs", "console.log('c'); export const z = 3;\n"),
            ("entry2.mjs", "import * as na from './a2.mjs'; console.log(Object.keys(na).join());\n"),
        ],
    );
    assert_eq!(fx.module_ok("entry.mjs"), "b\nc 2\nx,y,z");
    assert_eq!(fx.module_ok("entry2.mjs"), "b\nc\na\nx,y,z");
}

#[test]
fn long_property_keys_never_enter_the_shape_table() {
    if std::env::var_os(CHILD_ENV).is_some() || std::env::var_os(CASE_ENV).is_some() {
        return;
    }
    // The table is thread-local and outlives the heap meter, so a guest that
    // mints a long unique first key per dropped object must not grow it. The
    // first VM on this thread mints the realm's own shapes: count after it.
    assert_eq!(out("console.log(Object.keys({ a: 1 }).length)"), "1");
    let (before, _, _) = zipp_vm::shape_stats();
    assert_eq!(
        out(r#"
            const base = "k".repeat(4096);
            let sum = 0;
            for (let i = 0; i < 200; i++) {
                const o = {};
                const key = String(i).padStart(4, "0") + base;
                o[key] = i;
                o.after = i + 1;
                sum += o[key] + o.after;
                delete o.after;
                if (Object.keys(o).length !== 1 || o[key] !== i) throw new Error("bad " + i);
            }
            const p = { ["x".repeat(300)]: 1, y: 2 };
            p.z = 3;
            console.log(sum, Object.keys(p).map(k => k.length).join());
        "#),
        "40000 300,1,1"
    );
    let (after, _, _) = zipp_vm::shape_stats();
    assert!(
        after - before < 16,
        "long keys minted {} shape nodes",
        after - before
    );
}
