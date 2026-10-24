//! An array element redefined in the side table (an accessor above all)
//! overrides its dense slot, which still holds the old value. Every reader
//! that copies the dense store must decline such an array to [[Get]]: apply's
//! CreateListFromArrayLike (generic and interpreter lanes) and spread did not,
//! and returned the stale value. The results must be Node's for spread calls
//! and spread arrays, `apply` with a plain function and a native,
//! Reflect.apply and Reflect.construct, slice, concat, Array.from, join,
//! forEach, map, indexOf, includes, for-of, destructuring and JSON; and for a
//! getter that grows or shrinks the array mid-spread, a non-writable element,
//! and a hole beside a getter.

const SOURCE: &str = r##"
(function () {
  function mk() { var a = [1, 2, 3]; Object.defineProperty(a, 1, { get: function () { return 50; }, configurable: true }); return a; }
  function f() { return Array.prototype.join.call(arguments, ','); }
  console.log('spread call', f.apply(null, [].concat(mk())), f(...mk()));
  console.log('spread array', [...mk()].join(','));
  console.log('apply plain fn', f.apply(null, mk()));
  console.log('apply native', Math.max.apply(null, mk()));
  console.log('Reflect.apply', Reflect.apply(f, null, mk()));
  console.log('construct', Reflect.construct(function () { this.s = Array.prototype.join.call(arguments, ','); }, mk()).s);
  console.log('slice', mk().slice().join(','), 'concat', [].concat(mk()).join(','));
  console.log('from', Array.from(mk()).join(','), 'join', mk().join(','));
  console.log('forEach', (function () { var o = []; mk().forEach(function (v) { o.push(v); }); return o.join(','); })());
  console.log('map', mk().map(function (v) { return v; }).join(','), 'indexOf', mk().indexOf(50), 'includes', mk().includes(50));
  console.log('for-of', (function () { var o = []; for (var v of mk()) o.push(v); return o.join(','); })());
  console.log('destructure', (function () { var [x, y] = mk(); return x + ',' + y; })());
  console.log('JSON', JSON.stringify(mk()));
  var grow = [1, 2, 3];
  Object.defineProperty(grow, 0, { get: function () { if (grow.length < 6) grow.push(grow.length * 10); return 'g'; }, configurable: true });
  console.log('spread growing', [...grow].join(','));
  var shrink = [1, 2, 3, 4];
  Object.defineProperty(shrink, 1, { get: function () { shrink.length = 2; return 's'; }, configurable: true });
  console.log('spread shrinking', [...shrink].join(','), shrink.length);
  var nw = [1, 2];
  Object.defineProperty(nw, 0, { value: 9, writable: false });
  console.log('spread nonwritable', [...nw].join(','), Math.max.apply(null, nw), f(...nw));
  var holeGetter = [1, , 3];
  Object.defineProperty(holeGetter, 0, { get: function () { return 'h'; }, configurable: true });
  console.log('spread hole + getter', [...holeGetter].join(','), f.apply(null, holeGetter));
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
fn array_getter_element_worker() {
    if std::env::var_os("ZIPP_ARRAY_GETTER_ELEMENT_WORKER").is_none() {
        return;
    }
    // The harness has printed `test array_getter_element_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn array_getter_element_match_node() {
    if std::env::var_os("ZIPP_ARRAY_GETTER_ELEMENT_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["array_getter_element_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_ARRAY_GETTER_ELEMENT_WORKER", "1")
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
