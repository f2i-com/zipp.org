//! Non-ASCII string indexing goes through a position memo (`JsStr::seek`):
//! every unit-addressed read — `charCodeAt`, `codePointAt`, `s[i]`, `charAt`,
//! `at`, `slice`, `substring` — must answer exactly as a from-byte-zero decode
//! would, whatever order the positions are visited in, across surrogate
//! halves, lone surrogates, in-place appends and seam merges.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const HARNESS: &str = r#"
  function fail(msg) { throw new Error(msg); }
  function fromUnits(units) {
    var s = '';
    for (var i = 0; i < units.length; i++) s += String.fromCharCode(units[i]);
    return s;
  }
  // Reference answers computed from the unit array alone.
  function refCodePointAt(units, i) {
    var u = units[i];
    if (u >= 0xD800 && u <= 0xDBFF && i + 1 < units.length) {
      var lo = units[i + 1];
      if (lo >= 0xDC00 && lo <= 0xDFFF) return 0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00);
    }
    return u;
  }
  var seed = 1;
  function rnd(n) { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n; }
  function checkAt(label, s, units, i) {
    var n = units.length;
    var inRange = i >= 0 && i < n;
    var cc = s.charCodeAt(i);
    if (inRange ? cc !== units[i] : cc === cc) fail(label + ' charCodeAt(' + i + ') = ' + cc);
    var cp = s.codePointAt(i);
    if (inRange ? cp !== refCodePointAt(units, i) : cp !== undefined) fail(label + ' codePointAt(' + i + ') = ' + cp);
    var idx = s[i];
    if (inRange ? idx !== String.fromCharCode(units[i]) : idx !== undefined) fail(label + ' [' + i + ']');
    var ca = s.charAt(i);
    if (inRange ? ca !== String.fromCharCode(units[i]) : ca !== '') fail(label + ' charAt(' + i + ')');
    if (inRange) {
      var neg = s.at(i - n);
      if (neg !== String.fromCharCode(units[i])) fail(label + ' at(' + (i - n) + ')');
    }
  }
  function checkSlice(label, s, units, a, b) {
    var got = s.slice(a, b);
    var lo = a < 0 ? Math.max(units.length + a, 0) : Math.min(a, units.length);
    var hi = b < 0 ? Math.max(units.length + b, 0) : Math.min(b, units.length);
    var want = lo < hi ? fromUnits(units.slice(lo, hi)) : '';
    if (got !== want) fail(label + ' slice(' + a + ',' + b + ') length ' + got.length + ' want ' + want.length);
    if (got.length !== want.length) fail(label + ' slice length');
    if (a >= 0 && b >= 0) {
      var sub = s.substring(a, b);
      var x = Math.min(Math.max(a, 0), units.length), y = Math.min(Math.max(b, 0), units.length);
      var lo2 = Math.min(x, y), hi2 = Math.max(x, y);
      var want2 = fromUnits(units.slice(lo2, hi2));
      if (sub !== want2) fail(label + ' substring(' + a + ',' + b + ')');
    }
  }
  function checkAllOrders(label, s, units) {
    var n = units.length;
    if (s.length !== n) fail(label + ' length ' + s.length + ' != ' + n);
    var i;
    for (i = 0; i < n; i++) checkAt(label + ' fwd', s, units, i);
    for (i = n - 1; i >= 0; i--) checkAt(label + ' bwd', s, units, i);
    for (i = 0; i < n; i++) checkAt(label + ' alt', s, units, (i & 1) ? n - 1 - (i >> 1) : (i >> 1));
    for (i = 0; i < 3 * n; i++) checkAt(label + ' rnd', s, units, rnd(n + 2) - 1);
    for (i = 0; i < n; i += 7) { checkAt(label + ' rep', s, units, i); checkAt(label + ' rep2', s, units, i); }
    checkAt(label + ' end', s, units, n);
    checkAt(label + ' past', s, units, n + 5);
  }
  function checkAllSlices(label, s, units) {
    var n = units.length;
    for (var a = -2; a <= n + 2; a++) for (var b = -2; b <= n + 2; b++) checkSlice(label, s, units, a, b);
    // One-unit slices in both directions interleaved with reads.
    for (var i = 0; i < n; i++) { checkSlice(label + ' one', s, units, i, i + 1); checkAt(label + ' one', s, units, i); }
    for (var j = n - 1; j >= 0; j--) checkSlice(label + ' onerev', s, units, j, j + 1);
  }
"#;

#[test]
fn unit_reads_and_slices_match_reference_in_every_order() {
    let src = format!(
        "{HARNESS}
  var H = 0xD83D, L = 0xDE00; // U+1F600 as a surrogate pair
  var corpora = {{
    bmp: [0x61, 0xE9, 0x4E2D, 0x62, 0xFC, 0x416, 0x63, 0x7FF, 0x800, 0xFFFF, 0x64],
    astral: [0x61, H, L, 0x62, H, L + 1, 0x63, 0x1F600 >> 10, H, L],
    loneHigh: [0xD800, 0x61, 0xD800, 0xE9, 0xDBFF],
    loneLow: [0xDC00, 0x61, 0xDFFF, 0xE9, 0xDC00],
    mixed: [0x61, 0xE9, H, L, 0xD800, 0x62, 0x4E2D, 0xDC00, 0x7A, H, L, 0xDBFF, 0xDC00, 0x41],
    tail: [0x41, 0x42, H],
    head: [L, 0x43, 0x44]
  }};
  for (var name in corpora) {{
    var units = corpora[name];
    var s = fromUnits(units);
    checkAllOrders(name, s, units);
    checkAllSlices(name, s, units);
  }}
  // A longer string: sequential, reversed, strided and random access.
  var long = [];
  for (var k = 0; k < 600; k++) {{
    var m = k % 5;
    if (m === 0) long.push(0x61 + (k % 26));
    else if (m === 1) long.push(0xE9);
    else if (m === 2) long.push(0x4E2D);
    else if (m === 3) {{ long.push(H); long.push(L); }}
    else long.push(0xD800 + (k % 7));
  }}
  var ls = fromUnits(long);
  checkAllOrders('long', ls, long);
  for (var q = 0; q < 400; q++) {{
    var a = rnd(long.length + 1), b = rnd(long.length + 1);
    checkSlice('long', ls, long, a, b);
  }}
  for (var w = 0; w < long.length; w += 13) checkSlice('long window', ls, long, w, Math.min(w + 9, long.length));
  console.log('ok');
"
    );
    assert_eq!(run_ok(&src), vec!["ok"]);
}

#[test]
fn in_place_appends_and_seam_merges_keep_reads_exact() {
    let src = format!(
        "{HARNESS}
  var H = 0xD83D, L = 0xDE00;
  // Build by in-place `+=` while reading between appends, so the memo is
  // parked at the old end when the tail changes underneath it.
  var units = [];
  var s = '';
  var pieces = [[0x61], [0xE9], [H], [L], [0x4E2D, 0x62], [0xD800], [0xDC00], [H, L], [0x7A]];
  for (var round = 0; round < 40; round++) {{
    var piece = pieces[round % pieces.length];
    // Read the tail first (parks the memo at the last code point / the end).
    if (units.length) {{ checkAt('tail', s, units, units.length - 1); }}
    s.slice(Math.max(units.length - 2, 0), units.length);
    s += fromUnits(piece);
    for (var i = 0; i < piece.length; i++) units.push(piece[i]);
    // The new tail, the old tail (a merged seam sits across them) and the head.
    checkAt('new tail', s, units, units.length - 1);
    if (units.length >= 2) checkAt('old tail', s, units, units.length - 2);
    if (units.length >= 3) checkAt('older tail', s, units, units.length - 3);
    checkAt('head', s, units, 0);
    checkSlice('tail slice', s, units, Math.max(units.length - 3, 0), units.length);
  }}
  checkAllOrders('built', s, units);
  checkAllSlices('built', s, units);
  // A seam merge exactly at the memo: lone high at the end, then a lone low.
  var t = fromUnits([0x61, 0xE9, H]);
  var tu = [0x61, 0xE9, H];
  checkAt('pre-seam', t, tu, 2);
  t += String.fromCharCode(L);
  tu.push(L);
  if (t.codePointAt(2) !== 0x1F600) fail('seam merge codePointAt ' + t.codePointAt(2));
  checkAllOrders('seam', t, tu);
  checkAllSlices('seam', t, tu);
  // Concatenation results (fresh strings) index independently of their parts.
  var left = fromUnits([0x62, H]);
  var right = fromUnits([L, 0x63]);
  left.charCodeAt(1); right.charCodeAt(0);
  var joined = left + right;
  checkAllOrders('joined', joined, [0x62, H, L, 0x63]);
  checkAllSlices('joined', joined, [0x62, H, L, 0x63]);
  console.log('ok');
"
    );
    assert_eq!(run_ok(&src), vec!["ok"]);
}

/// Release builds only: the workspace test convention is `--release`, and a
/// debug-profile interpreter is slow enough to blur the bound.
#[cfg(not(debug_assertions))]
#[test]
fn sequential_non_ascii_scan_is_linear() {
    // 64K units of BMP text: quadratic decoding took seconds; the memo makes
    // each step O(1). The bound is generous so a loaded machine cannot fail
    // it, while a quadratic regression (several seconds natively) would.
    let src = r#"
      var piece = 'abé中üxyЖ';
      var s = '';
      while (s.length < 65536) s += piece;
      var t = 0;
      for (var i = 0; i < s.length; i++) t = (t + s.charCodeAt(i)) | 0;
      for (var j = s.length - 1; j >= 0; j--) t = (t + s.codePointAt(j)) | 0;
      for (var k = 0; k < s.length; k++) t = (t + s[k].length + s.slice(k, k + 1).length) | 0;
      console.log(t);
    "#;
    let started = std::time::Instant::now();
    let out = run_ok(src);
    assert_eq!(out.len(), 1);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(1500),
        "non-ASCII sequential scan took {:?}",
        started.elapsed()
    );
}
