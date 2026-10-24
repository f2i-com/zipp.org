//! `x == null` / `x != null` (either operand order) and the branch on it run
//! as one `FuseNullJump`: the null loaded, the test as `loose_eq_nullish`
//! decides it (nullish or `document.all`, with no coercion), the result and
//! the jump. The results must be Node's for every branch shape (`if`, `?:`,
//! `&&`, `||`, `!`, `while`, the bare value) over null, undefined, zeros,
//! empty strings, booleans, NaN, objects, arrays, an object whose
//! valueOf/toString must not run, functions, symbols, BigInts and wrappers;
//! for optional chaining, optional calls, nullish coalescing and defaults;
//! for a hot loop over mixed values; and when the result overwrites the
//! compared register.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  var calls = 0;
  var trap = { valueOf: function () { calls++; return null; }, toString: function () { calls++; return 'null'; } };
  var vals = [null, undefined, 0, -0, '', 'null', false, true, NaN, {}, [], [null], trap, function () {}, Symbol('s'), 0n, 1n, new Number(0), Object(null)];
  function name(v) { return typeof v === 'symbol' ? 'sym' : typeof v === 'bigint' ? v + 'n' : typeof v === 'function' ? 'fn' : Array.isArray(v) ? '[' + v.length + ']' : v === trap ? 'trap' : String(v); }

  // Every branch shape over every value.
  function eqIf(v) { if (v == null) return 'Y'; return 'n'; }
  function neIf(v) { if (v != null) return 'Y'; return 'n'; }
  function revIf(v) { if (null == v) return 'Y'; return 'n'; }
  function revNe(v) { if (null != v) return 'Y'; return 'n'; }
  function cond(v) { return v == null ? 'Y' : 'n'; }
  function condNe(v) { return v != null ? 'Y' : 'n'; }
  function andNe(v) { return (v != null && 'Y') || 'n'; }
  function orEq(v) { return (v == null || v === 0) ? 'Y' : 'n'; }
  function notEq(v) { return !(v == null) ? 'Y' : 'n'; }
  function whileNe(v) { var k = 0; while (v != null && k < 2) k++; return 'k' + k; }
  function asValue(v) { var b = v == null; var c = v != null; return (b ? 'T' : 'F') + (c ? 'T' : 'F'); }
  var fns = [eqIf, neIf, revIf, revNe, cond, condNe, andNe, orEq, notEq, whileNe, asValue];
  fns.forEach(function (f) { log(f.name, vals.map(function (v) { return name(v) + '=' + f(v); }).join(' ')); });
  log('no coercion', calls);

  // Optional chaining and nullish coalescing around it.
  var objs = [null, undefined, { a: { b: 1 } }, { a: null }, { a: { b: null } }, 0, ''];
  log('optional', objs.map(function (o) { return String(o?.a?.b); }).join(','));
  log('optional call', objs.map(function (o) { return String(o?.a?.toString?.()); }).join(','));
  log('coalesce', objs.map(function (o) { return String((o ?? 'D') === o); }).join(','));
  function def(x) { if (x == null) x = 'dflt'; return x; }
  log('default', [null, undefined, 0, false, 'v'].map(def).join(','));

  // A hot loop over mixed values at one site.
  var mixed = [null, 1, undefined, 'a', {}, 0, false, null];
  var cnt = 0;
  for (var i = 0; i < 4000; i++) { if (mixed[i & 7] != null) cnt++; }
  log('loop', cnt);

  // The compared register itself overwritten by the result.
  var r = null;
  r = r == null;
  var q = 5;
  q = q != null;
  log('self overwrite', r + ',' + q);

  out.forEach(function (l) { console.log(l); });
})();
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

fn zipp_lines() -> Vec<String> {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.join("\n").lines().map(str::to_owned).collect()
}

#[test]
fn null_jump_fused_worker() {
    if std::env::var_os("ZIPP_NULL_JUMP_FUSED_WORKER").is_none() {
        return;
    }
    // The harness has printed `test null_jump_fused_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn null_jump_fused_match_node() {
    if std::env::var_os("ZIPP_NULL_JUMP_FUSED_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["null_jump_fused_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_NULL_JUMP_FUSED_WORKER", "1")
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("spawn the worker");
    assert!(out.status.success(), "worker failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("LINE ").map(str::to_owned))
        .collect();
    assert_eq!(got, want, "interpreter-only run");
}
