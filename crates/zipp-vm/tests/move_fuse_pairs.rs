//! `UpvalGet; Move` (a callee read from the closure, then an argument copied
//! into the call's window) and `Move; Move` (an argument window's copies) run
//! as one dispatch each (`FuseUpvalGetMove`, `FuseMove2`), in the main and
//! the lite loop. The results must be Node's for a callee read in its TDZ
//! (const and let) and after, argument windows of one to six values in and
//! out of order, a callee held in a closure, recursion, swaps and rotations
//! through temporaries, argument evaluation order, and a hot loop.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }

  // A callee read from the closure while still in its TDZ throws, through the
  // fused read-and-copy, and works once initialised.
  function outer() {
    function inner(p) { return fn(p, p); }
    var r;
    try { r = inner(1); } catch (e) { r = e.constructor.name; }
    const fn = function (a, b) { return a + b; };
    return r + ',' + inner(2);
  }
  log('tdz callee', outer());
  function outerLet() {
    function inner(p, q) { return add(p, q) + add(q, p); }
    var first;
    try { first = inner(1, 2); } catch (e) { first = e instanceof ReferenceError; }
    let add = (a, b) => a * 10 + b;
    return first + ',' + inner(1, 2);
  }
  log('tdz let', outerLet());

  // Argument windows of every width, nested calls, recursion, spread neighbours.
  function sum() { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s; }
  function windows(a, b, c, d, e, f) {
    return [sum(a), sum(a, b), sum(a, b, c), sum(a, b, c, d), sum(a, b, c, d, e), sum(a, b, c, d, e, f), sum(b, a, d, c, f, e)].join(',');
  }
  log('windows', windows(1, 2, 3, 4, 5, 6));
  var helper = function (x, y) { return x - y; };
  function viaUpval(x, y) { return helper(x, y) + helper(y, x) * 2; }
  log('upval callee', viaUpval(7, 3));
  function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
  log('recursion', fib(20));
  function swap(a, b) { var t = a; a = b; b = t; return a + ':' + b; }
  log('swap', swap('x', 'y'));
  function rot(a, b, c) { var t = a; a = b; b = c; c = t; return [a, b, c].join(''); }
  log('rotate', rot('a', 'b', 'c'));
  var calls = [];
  function rec(tag) { calls.push(tag); return tag; }
  function order(a, b) { return rec(a) + rec(b) + rec(a); }
  log('order', order('1', '2') + ' ' + calls.join(''));
  var acc = 0;
  for (var i = 0; i < 1000; i++) acc += viaUpval(i, 1);
  log('loop', acc);

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
fn move_fuse_pairs_worker() {
    if std::env::var_os("ZIPP_MOVE_FUSE_PAIRS_WORKER").is_none() {
        return;
    }
    // The harness has printed `test move_fuse_pairs_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn move_fuse_pairs_match_node() {
    if std::env::var_os("ZIPP_MOVE_FUSE_PAIRS_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["move_fuse_pairs_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_MOVE_FUSE_PAIRS_WORKER", "1")
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
