//! A run of four or more integer `switch` cases (from its second case, as the
//! string cases' `FuseStrSwitch`) runs as one `FuseIntSwitch`: one lookup of
//! the discriminant in a table of the run's literals, dense by value when they
//! cluster and hashed otherwise, with the first case of each value kept. The
//! results must be Node's for dense and sparse cases, a default in the middle,
//! fallthrough, duplicate cases, negatives and the i32 extremes, a discriminant
//! that is a double equal to a case, -0, NaN, Infinity, out-of-range numbers,
//! strings, null, undefined, booleans, objects, BigInts, a run broken by a
//! non-literal or a string case, a discriminant with a side effect, nested
//! switches, break, continue and labelled break in a loop, and values whose
//! type changes between calls at one site.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }

  // Dense cases, with a default in the middle and fallthrough.
  function dense(k) {
    switch (k) {
      case 0: return 'zero';
      case 1: return 'one';
      case 2:
      case 3: return 'two-three';
      case 4: return 'four';
      default: return 'default';
      case 5: return 'five';
      case 6: k = 'six-'; // falls through
      case 7: return 'seven:' + k;
      case 8: return 'eight';
      case 2: return 'dup two (never)';
    }
  }
  var keys = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, -1, 3.0, 2.5, -0, NaN, Infinity, '3', null, undefined, true, [3], { valueOf: function () { return 3; } }, 4294967298, 2147483647];
  log('dense', keys.map(function (k) { return dense(k); }).join(','));
  log('bigint', dense(3n));

  // Sparse values (a hashed table), negatives and the i32 extremes.
  function sparse(k) {
    switch (k) {
      case -2147483648: return 'min';
      case -7: return 'neg7';
      case 1: return 'one';
      case 1000: return 'k';
      case 100000: return 'hk';
      case 2147483647: return 'max';
      case 31337: return 'leet';
      case 1000: return 'dup';
    }
    return 'none';
  }
  var sk = [-2147483648, -7, 1, 1000, 100000, 2147483647, 31337, 999, 0, -2147483649, 2147483648, 1000.0, 1e3, '1000', -7.0, -0];
  log('sparse', sk.map(sparse).join(','));

  // Long runs, as a compiler's node-kind switch has them.
  function kind(k) {
    switch (k) {
      case 10: case 11: case 12: return 'a';
      case 13: return 'b';
      case 14: case 15: return 'c';
      case 16: return 'd';
      case 17: return 'e';
      case 18: case 19: case 20: case 21: case 22: return 'f';
      case 23: return 'g';
      case 24: return 'h';
      case 25: return 'i';
      case 26: case 27: return 'j';
      case 28: return 'k';
      case 29: return 'l';
      case 30: return 'm';
      default: return '-';
    }
  }
  var hist = {};
  for (var i = 0; i < 2000; i++) {
    var r = kind(i % 40);
    hist[r] = (hist[r] || 0) + 1;
  }
  log('kind hist', Object.keys(hist).sort().map(function (k) { return k + '=' + hist[k]; }).join(' '));
  log('kind doubles', [10.0, 12.5, 30.0, -0].map(kind).join(','));

  // A switch whose cases mix literals with non-literals and strings.
  var K = 5;
  function mixed(k) {
    switch (k) {
      case 1: return 'i1';
      case 2: return 'i2';
      case K: return 'K';
      case 3: return 'i3';
      case '4': return 's4';
      case 4: return 'i4';
      case 6: return 'i6';
      case 7: return 'i7';
      case 8: return 'i8';
      case 9: return 'i9';
    }
    return 'none';
  }
  log('mixed', [1, 2, 3, 4, '4', 5, 6, 7, 8, 9, 10].map(mixed).join(','));

  // A side effect in the discriminant runs once.
  var calls = 0;
  function disc() { calls++; return 6; }
  switch (disc()) { case 1: case 2: case 3: case 4: case 5: break; case 6: log('side effect', 'six ' + calls); break; case 7: break; }

  // Nested switches, a switch in a loop with break/continue, labels.
  var acc = [];
  outer: for (var j = 0; j < 12; j++) {
    switch (j % 6) {
      case 0: acc.push('a'); break;
      case 1: acc.push('b'); continue;
      case 2:
        switch (j) { case 2: acc.push('c2'); break; case 8: acc.push('c8'); break; case 14: acc.push('c14'); break; case 20: break; case 26: break; }
        break;
      case 3: acc.push('d'); break;
      case 4: if (j > 6) break outer; acc.push('e'); break;
      case 5: acc.push('f'); break;
    }
    acc.push('|');
  }
  log('loop', acc.join(''));

  // Values that change type between calls at one site.
  var seq = [1, '1', 1.0, 2, 2.0000001, 3, null, 4];
  function cnt(k) { switch (k) { case 1: return 1; case 2: return 2; case 3: return 3; case 4: return 4; case 5: return 5; } return 0; }
  log('types', seq.map(cnt).join(','));

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
fn int_switch_table_worker() {
    if std::env::var_os("ZIPP_INT_SWITCH_TABLE_WORKER").is_none() {
        return;
    }
    // The harness has printed `test int_switch_table_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn int_switch_table_match_node() {
    if std::env::var_os("ZIPP_INT_SWITCH_TABLE_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["int_switch_table_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_INT_SWITCH_TABLE_WORKER", "1")
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
