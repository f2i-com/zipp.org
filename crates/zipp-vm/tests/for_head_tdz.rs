//! A for-of / for-in head's lexical names in their dead zone while the head
//! expression runs: direct reads, member and call heads, `typeof`, closures and
//! classes made in the head (read then and later), direct `eval`, destructuring
//! and for-in, with heads that can capture nothing compiled without a cell
//! (`head_captures_nothing`). Run interpreted, at the default thresholds, with
//! every function compiled at once and under GC stress; the expected lines are
//! Node 24's.

const EXPECTED: &str = "self ReferenceError
array ReferenceError
member ReferenceError
call ReferenceError
typeof ReferenceError
closure-now ReferenceError
closure-later 3
closure-call ReferenceError
class 2
class-call ReferenceError
eval ReferenceError
forin-self ReferenceError
forin-obj a
plain 6
destructure 14
destructure-self ReferenceError
map 10
let-cond 6
let-cond-dead ReferenceError
hot 120000";

#[test]
fn for_head_tdz_child() {
    if std::env::var_os("ZIPP_FOR_HEAD_TDZ_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn for_heads_keep_their_dead_zone() {
    if std::env::var_os("ZIPP_FOR_HEAD_TDZ_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "for_head_tdz_child", "--nocapture"])
            .env("ZIPP_FOR_HEAD_TDZ_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// A for-of / for-in head's lexical names are in their dead zone while the
// head expression runs: a direct read throws, a closure, class or direct
// eval made there sees the dead binding (and throws when it reads it later),
// and a head that can capture nothing needs no runtime cell at all.
const out = [];
const x = [7, 8];
const o = { items: [1, 2, 3], x: { a: 1 } };
function attempt(name, f) {
  try {
    out.push(name + " " + f());
  } catch (e) {
    out.push(name + " " + e.constructor.name);
  }
}
attempt("self", () => { let n = 0; for (const x of x) n += x; return n; });
attempt("array", () => { let n = 0; for (const x of [x]) n++; return n; });
attempt("member", () => { let n = 0; for (const x of o[x]) n++; return n; });
attempt("call", () => { let n = 0; for (const x of Array.from(x)) n++; return n; });
attempt("typeof", () => { let n = ""; for (const x of [typeof x]) n += x; return n; });
attempt("closure-now", () => { let n = 0; for (const x of (() => x)()) n++; return n; });
let later;
attempt("closure-later", () => { let n = 0; for (const x of ((later = () => x), [1, 2])) n += x; return n; });
attempt("closure-call", () => later());
let K;
attempt("class", () => { let n = 0; for (const x of [(K = class { m() { return x; } }), 2]) n++; return n; });
attempt("class-call", () => new K().m());
attempt("eval", () => { let n = 0; for (const x of eval("x")) n++; return n; });
attempt("forin-self", () => { let n = ""; for (const x in x) n += x; return n; });
attempt("forin-obj", () => { let n = ""; for (const k in o.x) n += k; return n; });
attempt("plain", () => { let n = 0; for (const v of o.items) n += v; return n; });
attempt("destructure", () => { let n = 0; for (const [a, b] of [[1, 2], [3, 4]]) n += a * b; return n; });
attempt("destructure-self", () => { let n = 0; for (const [a, x] of [[1, x]]) n += a; return n; });
attempt("map", () => { let n = 0; for (const [k, v] of new Map([[1, 2], [3, 4]])) n += k + v; return n; });
attempt("let-cond", () => { let n = 0; for (let x of (o.items.length > 2 ? o.items : x)) n += x; return n; });
attempt("let-cond-dead", () => { let n = 0; for (let x of (o.items.length > 9 ? o.items : x)) n += x; return n; });
function hot(list) {
  let t = 0;
  for (const v of list) t += v;
  return t;
}
let s = 0;
for (let i = 0; i < 20000; i++) s += hot(o.items);
out.push("hot " + s);
console.log(out.join("\n"));
"####;
