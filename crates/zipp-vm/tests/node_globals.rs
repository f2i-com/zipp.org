//! Node's globals for a program run from a file (`lang/js/node`).
//!
//! The command switches them on for a file run (`zipp app.js`); here the
//! library is asked directly. The program writes partial lines, orders
//! `process.nextTick`, `queueMicrotask`, promise jobs, cleared timeouts and
//! intervals, clones a cyclic graph of every kind `structuredClone` copies,
//! reads and edits a URL and its search parameters, and sets
//! `process.exitCode` with an `exit` listener. The expected lines
//! are Node 24's (`node app.js a b`).

#[test]
fn node_globals_behave_as_node() {
    zipp_vm::set_node_runtime(
        true,
        vec!["zipp".into(), "app.js".into(), "a".into(), "b".into()],
    );
    let outcome = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(outcome.error, None);
    let lines: Vec<&str> = outcome.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        [
            r#"partial-joined"#,
            r#"a"#,
            r#"b"#,
            r#"object true object a,b bigint true"#,
            r#"global,queueMicrotask,setTimeout,structuredClone"#,
            r#"tick 1,micro,promise,promise2,tick2,iv0,iv1,iv2,t1 xy"#,
            r#"true true 0 gi 2 true false 10 3 true boom"#,
            r#"DataCloneError: () => 1 could not be cloned."#,
            r#"DataCloneError: Symbol(s) could not be cloned."#,
            r#"https://example.com/a/c?x=42&z=%C3%A9+%26%3D https://example.com x,42|z,é &= false TypeError ERR_INVALID_URL true"#,
            r#"exit 3"#,
        ]
    );
    assert_eq!(zipp_vm::node_exit_code(), 3);
}

const PROGRAM: &str = r####"const out = [];
process.stdout.write("partial-");
console.log("joined");
process.stdout.write("a\nb\n");
out.push([typeof process, global === globalThis, typeof process.env, process.argv.slice(2).join(","), typeof process.hrtime.bigint(), process.version.startsWith("v")].join(" "));
out.push(Object.keys(globalThis).filter(k => ["process", "global", "setTimeout", "structuredClone", "queueMicrotask"].includes(k)).sort().join(","));
const order = [];
queueMicrotask(() => order.push("micro"));
Promise.resolve().then(() => { order.push("promise"); process.nextTick(() => order.push("tick2")); }).then(() => order.push("promise2"));
process.nextTick((a) => order.push("tick " + a), 1);
const t1 = setTimeout((a, b) => order.push("t1 " + a + b), 60, "x", "y");
clearTimeout(setTimeout(() => order.push("never"), 1));
let n = 0;
const iv = setInterval(() => { order.push("iv" + n); if (++n === 3) clearInterval(iv); }, 1);
setTimeout(() => {
  out.push(order.join(","));
  const src = { a: 1, d: new Date(0), r: /x/gi, m: new Map([[1, { z: 2 }]]), s: new Set([1, 2]), arr: [1, , 3], big: 10n, u8: new Uint8Array([1, 2, 3]), err: new RangeError("boom") };
  src.self = src;
  const c = structuredClone(src);
  out.push([c !== src, c.self === c, c.d.getTime(), c.r.flags, c.m.get(1).z, c.s.has(2), 1 in c.arr, c.big, c.u8[2], c.err instanceof RangeError, c.err.message].join(" "));
  for (const bad of [() => 1, Symbol("s")]) { try { structuredClone(bad); } catch (e) { out.push(e.name + ": " + e.message); } }
  const u = new URL("https://EXAMPLE.com:443/a/b/../c?x=1&y=2#f");
  u.searchParams.append("z", "é &=");
  u.searchParams.set("x", "42");
  u.searchParams.delete("y");
  u.hash = "";
  let bad = null;
  try { new URL("nope"); } catch (e) { bad = e; }
  out.push([u.href, u.origin, [...u.searchParams].join("|"), Object.keys(globalThis).includes("URL"), bad.name, bad.code, URL.canParse("/x", u)].join(" "));
  process.exitCode = 3;
  process.on("exit", (code) => console.log("exit", code));
  console.log(out.join("\n"));
}, 120);
"####;
