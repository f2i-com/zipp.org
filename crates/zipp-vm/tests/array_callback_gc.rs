//! Collections inside an Array callback method.
//!
//! `map`, `filter`, `forEach`, `find`, `findIndex`, `some`, `every` and
//! `reduce` used to hold the collector off for their whole run, which
//! covered everything their callbacks did: `nodes.forEach(n => process(n))`
//! ran all of `process` for every node with no collection. They release it
//! around each callback now and root what they hold across one instead: the
//! callback, its `this`, the receiver, a species target, the results so far,
//! the element being asked about and the accumulator.
//!
//! These programs make each of those the only thing keeping a value alive
//! while a callback allocates enough to collect: a `thisArg` and a callback
//! nothing else holds, an element its own predicate removes from the array
//! (`filter` keeps it, `find` answers with it), results that exist only in
//! the result under construction, a reducer with no parameters. They run
//! with a collection at every safe point too (`ZIPP_GC_STRESS`), where a
//! value left unrooted is freed at once.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
function churn(n) { let keep = null; for (let i = 0; i < n; i++) keep = { i, a: [i, i + 1], s: "x" + i }; return keep.i; }
const N = 300;
// map: each result is a fresh object held only by the result under construction.
const mapped = Array.from({ length: N }, (_, i) => i).map((x) => { churn(40); return { v: x * 2, tag: "m" + x }; });
console.log(mapped.length, mapped[0].tag, mapped[N - 1].v, mapped.reduce((s, o) => s + o.v, 0));
// filter: the element is removed from the array by its own predicate, and kept.
const src = Array.from({ length: N }, (_, i) => ({ id: i, body: "b" + i }));
const kept = src.filter((o, i, arr) => { arr[i] = null; churn(40); return o.id % 3 === 0; });
console.log(kept.length, kept[1].body, kept[kept.length - 1].id, src.every((x) => x === null));
// forEach with a thisArg nothing else holds, and a callback nothing else holds.
let seen = 0;
Array.from({ length: N }, (_, i) => i).forEach(function (x) { churn(20); this.total += x; seen += this.total > 0 ? 1 : 0; }, { total: 0, name: "ctx" });
console.log(seen);
const ctxOut = (function () { const out = []; [1, 2, 3].forEach(function (x) { churn(200); out.push(this.name + x); }, { name: "ctx" }); return out.join(","); })();
console.log(ctxOut);
// reduce: a fresh accumulator each step, a reducer with no parameters, an array of arrays.
const acc = Array.from({ length: N }, (_, i) => i).reduce((a, x) => { churn(30); return { sum: a.sum + x, last: "l" + x }; }, { sum: 0, last: "" });
console.log(acc.sum, acc.last);
let calls = 0;
const none = [1, 2, 3, 4].reduce(function () { calls++; churn(300); return { fresh: calls }; });
console.log(calls, none.fresh);
const flat = Array.from({ length: 50 }, (_, i) => [i, "s" + i]).reduce((a, pair) => { churn(60); return a.concat([pair[1]]); }, []);
console.log(flat.length, flat[49]);
// find family: the answer is removed from the array by the predicate that accepts it.
const objs = Array.from({ length: N }, (_, i) => ({ id: i, note: "n" + i }));
const found = objs.find((o, i, arr) => { churn(40); if (o.id === 250) { arr[i] = undefined; arr.length = 10; return true; } return false; });
console.log(found.note, objs.length);
console.log([5, 6, 7].findIndex((x) => { churn(300); return x === 7; }), [1, 2].some((x) => { churn(300); return x > 1; }), [1, 2].every((x) => { churn(300); return x > 0; }));
// nested drivers, and a callback that throws partway.
const nested = Array.from({ length: 20 }, (_, i) => i).map((i) => Array.from({ length: 20 }, (_, j) => j).filter((j) => { churn(10); return (i + j) % 4 === 0; }).map((j) => "c" + i + "_" + j));
console.log(nested.length, nested[3].join("|"), nested[19].length);
try { [1, 2, 3].map((x) => { churn(100); if (x === 2) throw new Error("stop" + x); return { x }; }); } catch (e) { console.log(e.message); }
// a species target, and a sloppy callback with a primitive this.
class Sub extends Array {}
const sub = Sub.from([1, 2, 3, 4]).map((x) => { churn(100); return { x }; });
console.log(sub instanceof Sub, sub.length, sub[3].x);
const boxed = []; [1, 2].forEach(function () { churn(100); boxed.push(typeof this); }, "prim");
console.log(boxed.join(","));
// strings made in callbacks stay intact across later collections.
const strs = Array.from({ length: 200 }, (_, i) => i).map((i) => { churn(15); return "str-" + i + "-" + (i * 7); });
churn(5000);
console.log(strs[0], strs[199], strs.filter((s) => { churn(10); return s.length > 9; }).length);
// a hot compiled callback: the same shapes after the functions are compiled.
function hot(rounds) { let t = 0; for (let r = 0; r < rounds; r++) { const xs = Array.from({ length: 40 }, (_, i) => ({ k: i + r })); t += xs.map((o) => ({ k2: o.k * 2, pad: [o.k] })).filter((o) => o.k2 % 3 === 0).reduce((a, o) => a + o.k2 + o.pad.length, 0); xs.forEach((o) => { t += o.k & 1; }); if (xs.some((o) => o.k === -1) || !xs.every((o) => o.k >= r)) t = -1; t += xs.find((o) => o.k === r + 7).k; } return t; }
console.log(hot(400));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn array_callback_gc_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "300 m0 598 89700",
            "100 b3 297 true",
            "299",
            "ctx1,ctx2,ctx3",
            "44850 l299",
            "3 3",
            "50 s49",
            "n250 10",
            "2 true true",
            "20 c3_1|c3_5|c3_9|c3_13|c3_17 5",
            "stop2",
            "true 4 4",
            "object,object",
            "str-0-0 str-199-1393 185",
            "2432226",
        ]
    );
}

/// The same answers with a collection at every safe point, with the JIT
/// compiling at once, with both, and with no native code.
#[test]
fn array_callback_gc_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, vars) in [
        ("stress", &[("ZIPP_GC_STRESS", "1")][..]),
        ("eager", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("stress-eager", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("nojit-stress", &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..]),
        ("verify", &[("ZIPP_NURSERY_VERIFY", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("array_callback_gc_answers").arg("--nocapture");
        for (key, value) in vars {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:
{stdout}
{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
