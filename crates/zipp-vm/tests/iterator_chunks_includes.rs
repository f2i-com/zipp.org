//! `Iterator.prototype.chunks`, `windows` and `includes` (iterator chunking,
//! iterator includes), the `take`/`drop` limit bound and `Promise.try`'s
//! unwrapped result. Node 24 has none of the three methods, so the script
//! asserts its own expectations (the proposals' and Test262's). It runs in
//! the default mode, the interpreter, and under GC stress: a window buffer
//! once allocated before GetIteratorDirect's `next` getter was collected
//! while that getter ran.

const SOURCE: &str = r##"
var fails = [];
function eq(a, b, what) { var x = JSON.stringify(a), y = JSON.stringify(b); if (x !== y) fails.push(what + ": " + x + " !== " + y); }
function throwsType(f, T, what) { try { f(); fails.push(what + ": no throw"); } catch (e) { if (!(e instanceof T)) fails.push(what + ": " + e); } }
function* g(n) { for (var i = 0; i < n; i++) yield i; }

// chunks
eq(Array.from(g(7).chunks(3)), [[0, 1, 2], [3, 4, 5], [6]], "chunks partial");
eq(Array.from(g(6).chunks(3)), [[0, 1, 2], [3, 4, 5]], "chunks even");
eq(Array.from(g(0).chunks(2)), [], "chunks empty");
eq(Array.from(g(3).chunks(10)), [[0, 1, 2]], "chunks larger");
var cs = Array.from(g(4).chunks(1)); eq(cs, [[0], [1], [2], [3]], "chunks 1"); if (cs[0] === cs[1]) fails.push("chunks distinct");
[0, -0, -1, 2 ** 32].forEach(function (s) { throwsType(function () { g(3).chunks(s); }, RangeError, "chunks range " + s); });
[undefined, "2", 1.5, NaN, Infinity, null, {}].forEach(function (s) { throwsType(function () { g(3).chunks(s); }, TypeError, "chunks type " + String(s)); });

// windows
eq(Array.from(g(5).windows(2)), [[0, 1], [1, 2], [2, 3], [3, 4]], "windows 2");
eq(Array.from(g(5).windows(3)), [[0, 1, 2], [1, 2, 3], [2, 3, 4]], "windows 3");
eq(Array.from(g(2).windows(5)), [], "windows only-full");
eq(Array.from(g(2).windows(5, "allow-partial")), [[0, 1]], "windows allow-partial");
eq(Array.from(g(6).windows(2, "allow-partial")), [[0, 1], [1, 2], [2, 3], [3, 4], [4, 5]], "windows partial full");
["", "bad", null, 0, {}, new String("only-full")].forEach(function (u) { throwsType(function () { g(3).windows(1, u); }, TypeError, "windows undersized " + String(u)); });

// A plain iterator whose `next` getter allocates and collects: the helper's
// buffer must survive it.
function plain() {
  var count = 6;
  return { get next() { var junk = []; for (var i = 0; i < 2000; i++) junk.push({ i: i }); return function () { --count; return count >= 0 ? { done: false, value: count } : { done: true, value: undefined }; }; } };
}
var w = Iterator.prototype.windows.call(plain(), 2);
var first = w.next(); eq(first, { value: [5, 4], done: false }, "windows plain first");
eq(w.next(), { value: [4, 3], done: false }, "windows plain second");
var c = Iterator.prototype.chunks.call(plain(), 4);
eq([c.next(), c.next(), c.next()], [{ value: [5, 4, 3, 2], done: false }, { value: [1, 0], done: false }, { done: true }], "chunks plain");

// Validation closes the iterator before `next` is read; a closed helper does
// not forward return() again.
var closed = 0, nextReads = 0;
var closable = { __proto__: Iterator.prototype, get next() { nextReads++; return function () { return { done: true }; }; }, return: function () { closed++; return {}; } };
throwsType(function () { closable.chunks(0); }, RangeError, "chunks closes");
throwsType(function () { closable.windows(2, "x"); }, TypeError, "windows closes");
throwsType(function () { closable.includes(1, -1); }, RangeError, "includes closes");
throwsType(function () { closable.includes(1, 1.5); }, TypeError, "includes type closes");
eq([closed, nextReads], [4, 0], "closed before next");
var src = g(10), h = src.chunks(2); h.next(); var rets = 0; src.return = function () { rets++; return {}; }; h.return(); h.return();
eq(rets, 1, "chunks return once");

// includes
eq([[1, 2, 3].values().includes(2), [1, 2, 3].values().includes(4), [NaN].values().includes(NaN), [-0].values().includes(0)], [true, false, true, true], "includes basic");
eq([[4, 5, 6, 7].values().includes(5, 1), [4, 5, 6, 7].values().includes(5, 2), [4, 5].values().includes(4, Infinity), [4].values().includes(4, -0)], [true, false, false, true], "includes skip");
throwsType(function () { [1].values().includes(1, NaN); }, TypeError, "includes NaN");
throwsType(function () { [1].values().includes(1, 2 ** 53); }, RangeError, "includes too large");
var n = 0, shut = false;
var inf = { __proto__: Iterator.prototype, next: function () { return { done: false, value: ++n }; }, return: function () { shut = true; return {}; } };
eq([inf.includes(5), n, shut], [true, 5, true], "includes closes on match");

// take/drop bound, Promise.try unwrap
throwsType(function () { g(1).take(Number.MAX_SAFE_INTEGER + 1); }, RangeError, "take bound");
throwsType(function () { g(1).drop(Number.MAX_SAFE_INTEGER + 1); }, RangeError, "drop bound");
eq(Array.from(g(3).take(Infinity)), [0, 1, 2], "take infinity");
var p = Promise.resolve(1);
if (Promise.try(function () { return p; }) !== p) fails.push("Promise.try unwrap");
class Sub extends Promise {}
var sp = Sub.resolve(2);
if (Sub.try(function () { return sp; }) !== sp) fails.push("Promise.try subclass unwrap");
if (!(Sub.try(function () { throw 1; }) instanceof Sub)) fails.push("Promise.try subclass reject");
Sub.try(function () { throw 1; }).catch(function () {});
console.log(fails.length ? "FAIL\n" + fails.join("\n") : "ok");
"##;

#[test]
fn iterator_chunks_includes_worker() {
    if std::env::var_os("ZIPP_ITER_CHUNKS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), "ok");
}

#[test]
fn iterator_chunks_includes_all_modes() {
    if std::env::var_os("ZIPP_ITER_CHUNKS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, env) in [
        ("default", &[][..]),
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("gc-stress", &[("ZIPP_GC_STRESS", "1")][..]),
        ("interpreter-gc-stress", &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..]),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["iterator_chunks_includes_worker", "--exact", "--nocapture"])
            .env("ZIPP_ITER_CHUNKS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        for (k, v) in env {
            command.env(k, v);
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
