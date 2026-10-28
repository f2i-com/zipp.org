//! `[a, b] = [x, y]`: an array pattern assigned an array literal is
//! compiled to read the literal's elements directly while Array iteration is
//! the built-in one (`IterToArray`'s probe), and stepwise through the
//! iterator otherwise. Each case runs warm, so the JIT tiers see it, and the
//! expected lines are Node 24's: evaluation order of targets, defaults and
//! setters, nested patterns, the expression's value, a patched
//! `@@iterator`, `next` and `return`, generators, throws and dead zones.

#[test]
fn destructuring_an_array_literal_is_exact() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        [
            r#"swap 2,1"#,
            r#"rotate 2,3,1"#,
            r#"arr-swap 2,3,4,1"#,
            r#"value-used 2,2,1,2,1,true"#,
            r#"defaults 10,5"#,
            r#"default-not-mutating-value 7,"#,
            r#"longer-pattern 1,2,"#,
            r#"shorter-pattern 1"#,
            r#"holes-in-pattern 2,4"#,
            r#"nested 1,2,3"#,
            r#"obj-nested 9,8"#,
            r#"member-targets 3"#,
            r#"order rhs 1|rhs 2|ref a|set x A|ref b|set y B"#,
            r#"default-order ref|default|set 5"#,
            r#"patched-iterator 1,2,custom"#,
            r#"patched-next 1,2,2"#,
            r#"return-on-close 1,1"#,
            r#"after-restore 2,1"#,
            r#"throw-in-target threw RangeError boom"#,
            r#"tdz threw ReferenceError "#,
            r#"const-target threw TypeError Assignment to constant variable."#,
            r#"generator 30"#,
            r#"generator-in-target {"key":1,"z":2}"#,
            r#"string-source-unaffected xy"#,
            r#"spread-in-literal 7,8"#,
            r#"hole-in-literal ,2"#,
            r#"rest-pattern 1,2/3"#,
            r#"sum 50022901627"#,
        ]
    );
}

const PROGRAM: &str = r####"const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
function run(label, f) {
  try { log(label, f()); } catch (e) { log(label, "threw", e && e.constructor && e.constructor.name, label === "tdz" ? "" : e && e.message); }
}
// Warm each case so the JIT tiers see it too.
function many(label, f) { let r; for (let i = 0; i < 300; i++) r = f(); log(label, r); }

many("swap", () => { let a = 1, b = 2; [a, b] = [b, a]; return [a, b].join(); });
many("rotate", () => { let a = 1, b = 2, c = 3; [a, b, c] = [b, c, a]; return [a, b, c].join(); });
many("arr-swap", () => { const h = [1, 2, 3, 4]; for (let i = 0; i < 3; i++) [h[i], h[i + 1]] = [h[i + 1], h[i]]; return h.join(); });
many("value-used", () => { let a = 1, b = 2; const v = [a, b] = [b, a]; return [v.length, v[0], v[1], a, b, Array.isArray(v)].join(); });
many("defaults", () => { let a, b; [a = 10, b = 20] = [undefined, 5]; return [a, b].join(); });
many("default-not-mutating-value", () => { let a; const v = [a = 7] = [undefined]; return [a, v[0]].join(); });
many("longer-pattern", () => { let a, b, c; [a, b, c] = [1, 2]; return [a, b, c].join(); });
many("shorter-pattern", () => { let a; [a] = [1, 2, 3]; return a; });
many("holes-in-pattern", () => { let a, b; [, a, , b] = [1, 2, 3, 4]; return [a, b].join(); });
many("nested", () => { let a, b, c; [a, [b, c]] = [1, [2, 3]]; return [a, b, c].join(); });
many("obj-nested", () => { let a, b; [{ x: a }, b] = [{ x: 9 }, 8]; return [a, b].join(); });
many("member-targets", () => { const o = {}; [o.x, o["y"]] = [1, 2]; return o.x + o.y; });

run("order", () => {
  const seq = [];
  const tgt = { set x(v) { seq.push("set x " + v); }, set y(v) { seq.push("set y " + v); } };
  const get = (n, v) => (seq.push("rhs " + n), v);
  const ref = (n) => (seq.push("ref " + n), tgt);
  [ref("a").x, ref("b").y] = [get(1, "A"), get(2, "B")];
  return seq.join("|");
});
run("default-order", () => {
  const seq = [];
  const o = { set p(v) { seq.push("set " + v); } };
  const r = () => (seq.push("ref"), o);
  [r().p = (seq.push("default"), 5)] = [undefined];
  return seq.join("|");
});
run("patched-iterator", () => {
  const seq = [];
  const orig = Array.prototype[Symbol.iterator];
  Array.prototype[Symbol.iterator] = function* () { seq.push("custom"); yield* orig.call(this); };
  try { let a, b; [a, b] = [1, 2]; return [a, b, seq.join()].join(); } finally { Array.prototype[Symbol.iterator] = orig; }
});
run("patched-next", () => {
  const AIP = Object.getPrototypeOf([][Symbol.iterator]());
  const orig = AIP.next;
  let calls = 0;
  AIP.next = function () { calls++; return orig.call(this); };
  try { let a, b; [a, b] = [1, 2]; return [a, b, calls].join(); } finally { AIP.next = orig; }
});
run("return-on-close", () => {
  const AIP = Object.getPrototypeOf([][Symbol.iterator]());
  let closed = 0;
  AIP.return = function () { closed++; return {}; };
  try { let a; [a] = [1, 2]; return [a, closed].join(); } finally { delete AIP.return; }
});
run("after-restore", () => { let a = 1, b = 2; [a, b] = [b, a]; return [a, b].join(); });
run("throw-in-target", () => {
  let a;
  const bad = () => { throw new RangeError("boom"); };
  [a, bad().x] = [1, 2];
});
run("tdz", () => { [x] = [1]; let x; });
run("const-target", () => { const k = 1; [k] = [2]; });
run("generator", () => {
  function* g() { let a, b; [a, b] = [yield 1, yield 2]; return a + b; }
  const it = g(); it.next(); it.next(10); return it.next(20).value;
});
run("generator-in-target", () => {
  function* g() { const o = {}; [o[yield "k"], o.z] = [1, 2]; return JSON.stringify(o); }
  const it = g(); it.next(); return it.next("key").value;
});
run("string-source-unaffected", () => { let a, b; [a, b] = "xy"; return a + b; });
run("spread-in-literal", () => { let a, b; [a, b] = [...[7, 8]]; return [a, b].join(); });
run("hole-in-literal", () => { let a, b; [a, b] = [, 2]; return [a, b].join(); });
run("rest-pattern", () => { let a, r; [a, ...r] = [1, 2, 3]; return [a, r.join("/")].join(); });
run("sum", () => { let s = 0, x = 0, y = 1; for (let i = 0; i < 1e5; i++) { [x, y] = [y, (x + y) % 1000003]; s += x; } return s; });
console.log(out.join("\n"));
"####;
