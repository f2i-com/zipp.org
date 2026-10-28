//! `this.cmp(a, b)` (a GetProp, the arguments, CallWithThis) at compiled sites
//! whose comparator changes between phases, so their leaf inline and arms
//! name another callee and the general route (`jit_call_with_this_cross`)
//! runs: arrows, a sloppy and a strict function reading `this`, a comparator
//! that throws, a native comparator, and a String.prototype method called on
//! primitives. Run interpreted, at the default thresholds, with every function
//! compiled at once, under GC stress and with the cross call off; the
//! expected line is Node 24's.

const EXPECTED: &str = "605368161,831092184,681186738,538566336,143318780,670263712,618576619,24076131,754974833,121600864,354734062,217803104,481979526,775787672,336235281,802332756,954347282,988021229,506918281,176039207,193063337,359478656,787935514,741811089,0,371699794,36000,true";

#[test]
fn cwt_cross_fallback_child() {
    if std::env::var_os("ZIPP_CWT_CROSS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn captured_method_calls_match_node() {
    if std::env::var_os("ZIPP_CWT_CROSS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("cross-off", Some(("ZIPP_NO_CWT_CROSS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "cwt_cross_fallback_child", "--nocapture"])
            .env("ZIPP_CWT_CROSS_CHILD", "1")
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

const PROGRAM: &str = r####"// `this.cmp(a, b)` (a GetProp, the arguments, CallWithThis) at a compiled
// site whose comparator changes with the program's phase: arrows, a sloppy
// and a strict function reading `this`, a comparator that throws, a native,
// and a primitive receiver through a String.prototype method.
const out = [];
class PQ {
  constructor(cmp) { this.cmp = cmp; this.h = []; this.calls = 0; }
  push(v) { const h = this.h; h.push(v); let i = h.length - 1; while (i > 0) { const p = (i - 1) >> 1; if (this.cmp(h[i], h[p]) >= 0) break; const t = h[i]; h[i] = h[p]; h[p] = t; i = p; } }
  pop() { const h = this.h; const top = h[0]; const last = h.pop(); if (h.length > 0) { h[0] = last; let i = 0; for (;;) { const l = 2 * i + 1, r = l + 1; let m = i; if (l < h.length && this.cmp(h[l], h[m]) < 0) m = l; if (r < h.length && this.cmp(h[r], h[m]) < 0) m = r; if (m === i) break; const t = h[i]; h[i] = h[m]; h[m] = t; i = m; } } return top; }
}
function drain(q, n, seed) { let s = seed; for (let i = 0; i < n; i++) { s = (s * 1103515245 + 12345) % 2147483648; q.push(s % 10007); } let acc = 0, k = 0; while (q.h.length > 0) acc = (acc * 31 + q.pop() * (++k)) % 1000000007; return acc; }
out.push(drain(new PQ((a, b) => a - b), 3000, 1));
out.push(drain(new PQ((a, b) => b - a), 3000, 2));
out.push(drain(new PQ(function (a, b) { this.calls++; return a % 7 - b % 7 || a - b; }), 2000, 3));
const strictCmp = function (a, b) { "use strict"; return this instanceof PQ ? (a & 15) - (b & 15) || b - a : 0; };
out.push(drain(new PQ(strictCmp), 2000, 4));
let thrown = 0;
const boom = new PQ((a, b) => { if (a === 4242) throw new RangeError("x"); return a - b; });
for (let r = 0; r < 20; r++) { try { out.push(drain(boom, 200, r * 17)); } catch (e) { thrown++; boom.h.length = 0; } }
out.push(thrown);
const nat = new PQ(Math.min); try { out.push(drain(nat, 100, 5)); } catch (e) { out.push(e.constructor.name); }
String.prototype.twice = function (n) { return this.length * n; };
function lens(words) { let t = 0; for (const w of words) t += w.twice(2); return t; }
let t = 0; for (let r = 0; r < 3000; r++) t += lens(["a", "bb", "ccc"]);
out.push(t);
const counted = new PQ(function (a, b) { this.calls++; return a - b; });
drain(counted, 500, 9); out.push(counted.calls > 0);
console.log(out.join(","));
"####;
