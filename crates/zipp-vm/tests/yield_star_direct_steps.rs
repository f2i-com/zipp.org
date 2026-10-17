//! `yield*` into generators with the intrinsic `next`, which run directly
//! and pass plain values up, against everything that must still go through
//! the protocol: an inner generator whose `next` is replaced (own, on its
//! prototype, or by a new prototype), a user iterator whose result objects
//! are forwarded by identity, throw and return forwarding after direct steps,
//! inner throws, re-entry and completed inners. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function* acc(n) { var s = 0; for (var i = 0; i < n; i++) { var got = yield i; s += got === undefined ? 1 : got; } return s; }
function* two(n) { var a = yield* acc(n); var b = yield* acc(n >> 1); return a * 3 + b; }
function* three(n) { return (yield* two(n)) + 1; }
var g = three(6), r = g.next(), seen = [], sent = 0;
while (!r.done) { seen.push(r.value); sent = (sent + r.value) & 7; r = g.next(sent); }
log("sent values", seen.join(), r.value);
function tree(d, v) { return d ? { v: v, l: tree(d - 1, v * 2 + 1), r: tree(d - 1, v * 2 + 2) } : null; }
function* inorder(t) { if (!t) return 0; var l = yield* inorder(t.l); yield t.v; var rr = yield* inorder(t.r); return l + rr + 1; }
var vals = [], it = inorder(tree(6, 0)), x = it.next();
while (!x.done) { vals.push(x.value); x = it.next(); }
log("inorder", vals.length, vals.slice(0, 8).join(), x.value, [...inorder(tree(4, 1))].join());
var results = [];
var outerIt = (function* () { yield* acc(3); })();
for (var k = 0; k < 4; k++) { var res = outerIt.next(); results.push(Object.keys(res).join("/") + "=" + res.value + "," + res.done); }
log("result objects", results.join(" "));
function* replaceOwn() { var inner = acc(5); inner.next = function (v) { return { value: "own " + v, done: v === 2 }; }; return yield* inner; }
var ro = replaceOwn(); log("own next", ro.next().value, ro.next(1).value, JSON.stringify(ro.next(2)));
var gp = Object.getPrototypeOf(acc);
var genProto = Object.getPrototypeOf(acc(1));
var savedNext = Object.getPrototypeOf(genProto).next;
var calls = 0;
Object.getPrototypeOf(genProto).next = function (v) { calls++; return savedNext.call(this, v); };
var viaProto = [...(function* () { yield* acc(3); })()];
Object.getPrototypeOf(genProto).next = savedNext;
log("prototype next", viaProto.join(), calls > 0);
function* fresh() { yield 1; yield 2; }
var reproto = fresh();
Object.setPrototypeOf(reproto, { next: function () { return { value: "custom", done: this.d = !this.d }; }, [Symbol.iterator]: function () { return this; } });
log("replaced prototype", JSON.stringify([...(function* () { return yield* reproto; })()]));
var keep = { value: "kept", done: false, extra: 1 };
var userIt = { i: 0, [Symbol.iterator]: function () { return this; }, next: function () { return this.i++ < 2 ? keep : { value: "end", done: true }; } };
var wrap = (function* () { return yield* userIt; })();
var w1 = wrap.next();
log("identity", w1 === keep, w1.extra, wrap.next() === keep, JSON.stringify(wrap.next()));
function* withFinally() { try { yield "a"; yield "b"; } finally { log("inner finally"); } }
function* outerF() { try { yield* withFinally(); } catch (e) { log("outer caught", e); } }
var of1 = outerF(); of1.next(); log("throw forwarded", JSON.stringify(of1.throw("boom")));
var of2 = outerF(); of2.next(); of2.next(); log("return forwarded", JSON.stringify(of2.return(7)), JSON.stringify(of2.next()));
function* thrower() { yield 1; throw new RangeError("inner"); }
var tg = (function* () { yield* thrower(); })();
tg.next();
try { tg.next(); log("inner throw", "none"); } catch (e) { log("inner throw", e.constructor.name, e.message, JSON.stringify(tg.next())); }
var reenter;
function* selfCall() { yield 1; reenter.next(); }
reenter = (function* () { yield* selfCall(); })();
reenter.next();
try { reenter.next(); } catch (e) { log("re-entry", e.constructor.name); }
var done = acc(0); done.next();
log("completed inner", JSON.stringify([...(function* () { return yield* done; })()]), JSON.stringify((function* () { return yield* done; })().next()));
var gg = acc(3);
Object.defineProperty(gg, "next", { get: function () { log("next getter ran"); return function () { return { value: "from getter", done: true }; }; } });
log("next getter", JSON.stringify(gg.next()));
var two2 = acc(3);
log("extra arguments", JSON.stringify(two2.next(5, 6)), JSON.stringify(two2.next(7, 8, 9)), JSON.stringify(Object.freeze(acc(1)).next()));
var chainOut = [];
for (var v of (function* () { yield* [1, 2]; yield* "ab"; yield* new Set([3]); yield* acc(2); })()) chainOut.push(v);
log("mixed inners", chainOut.join());
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn yield_star_into_generators_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
