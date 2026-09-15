//! Arrays, array buffers and the array-shaped builtins (15 September 2026
//! audit, track E-arrays).
//!
//! * An array longer than `MAX_DENSE_ARRAY_LEN` (`new Array(n)`, `a.length =
//!   n`, a far index write) keeps only a prefix in its dense store. `fill`,
//!   `slice`, `join`, `at`, `concat`, spread, for-of and the rest sized their
//!   work from that store and silently answered for an empty or truncated
//!   array: a sieve over two million entries counted 0 primes. They now use
//!   the JS length, and throw a RangeError where a result cannot be built.
//! * `join`/`toString`/`String(array)` and the default `sort` order went
//!   through a lossy Rust `String`: surrogate halves in separate elements
//!   became U+FFFD (`s.split('').join('') !== s`) and sorting compared code
//!   points instead of UTF-16 code units.
//! * `Object.prototype.toString` recognised errors by their `name` string
//!   rather than by [[ErrorData]].
//! * `toSpliced` read non-Number arguments as 0; `includes`, `slice` and a
//!   comparator-driven `sort` used the live array rather than the length read
//!   at entry; `flat` copied holes; `at` ignored inherited indices; `concat`
//!   of a spreadable non-array defined absent indices.
//! * ArrayBuffer methods were dispatched by receiver kind, so a plain buffer
//!   answered `grow` (only SharedArrayBuffer has it) and a deleted prototype
//!   method still ran. `grow` and `transferToImmutable` also skipped the host
//!   pin check, so a guest could reallocate or detach a buffer the host holds
//!   a raw address into.
//! * `concat`/`flat` built their dense results with no heap preflight.
//!
//! The probes are self-checking and print `label=ok` per line. The small
//! shapes, and the arrays past the cap whose element paths differ by tier,
//! run in a child process per execution mode (default, interpreter, forced
//! JIT; GC stress for the small ones). The builtins over arrays past the cap
//! run the same native code in every tier and run once.

#[cfg(not(feature = "safe-sandbox"))]
fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        out.error,
        out.output
    );
    out.output
}

/// The self-checking harness: each `check` appends `label=ok` or a `FAIL`
/// line, printed together at the end.
#[cfg(not(feature = "safe-sandbox"))]
const HARNESS: &str = r#"
var lines = [];
function same(a, b) {
  if (a === b) return true;
  if (typeof a === 'number' && typeof b === 'number' && a !== a && b !== b) return true;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return false;
    for (var i = 0; i < a.length; i++) if (!same(a[i], b[i])) return false;
    return true;
  }
  return false;
}
function check(label, f, expected) {
  var actual;
  try { actual = f(); } catch (e) { actual = 'threw ' + e; }
  lines.push(label + (same(actual, expected) ? '=ok' : '=FAIL ' + JSON.stringify(actual) + ' want ' + JSON.stringify(expected)));
}
function units(s) {
  var out = [];
  for (var i = 0; i < s.length; i++) out.push(s.charCodeAt(i).toString(16));
  return out.join(' ');
}
function keyCount(o) { var n = 0; for (var k in o) n++; return n; }
function rangeError(f) { try { f(); return 'completed'; } catch (e) { return e instanceof RangeError; } }

"#;

/// Arrays past the dense cap whose element reads and writes differ by tier
/// (for-of, spread, destructuring, stores after a `fill`). `N` is one past
/// the cap, so every "virtual" array here is one the engine cannot hold
/// densely at creation.
#[cfg(not(feature = "safe-sandbox"))]
const PROBES_VIRTUAL_TIERS: &str = r#"
// ---- past the dense cap, through each tier's element paths ----
function edges() { var a = new Array(N); a[0] = 'a'; a[N - 1] = 'z'; return a; }
check('fill', function () { var a = new Array(N).fill(7); return [a[0], a[N - 1], 0 in a, a.length, Object.keys(a).length === N]; }, [7, 7, true, N, true]);
check('fill-then-write', function () { var dp = new Array(N).fill(0); dp[5] += 1; dp[N - 1] += 2; return [dp[5], dp[6], dp[N - 1]]; }, [1, 0, 2]);
check('sieve', function () {
  var n = N + 1;
  var p = new Array(n + 1).fill(true); p[0] = p[1] = false;
  for (var i = 2; i * i <= n; i++) if (p[i]) for (var j = i * i; j <= n; j += i) p[j] = false;
  var c = 0; for (var k = 0; k <= n; k++) if (p[k]) c++;
  var q = new Uint8Array(n + 1); q[0] = q[1] = 1;
  for (var i2 = 2; i2 * i2 <= n; i2++) if (!q[i2]) for (var j2 = i2 * i2; j2 <= n; j2 += i2) q[j2] = 1;
  var want = 0; for (var k2 = 0; k2 <= n; k2++) if (!q[k2]) want++;
  return c === want && want > 0;
}, true);
check('spread', function () { var a = edges(); var s = [...a]; return [s.length, s[N - 1], 1 in s]; }, [N, 'z', true]);
check('for-of', function () { var a = edges(); var c = 0, last; for (var x of a) { c++; last = x; } return [c, last]; }, [N, 'z']);
check('destructure', function () { var [x, , y] = new Array(N).fill(4); return [x, y]; }, [4, 4]);
check('typed-from-array', function () { return new Int32Array(new Array(N).fill(3))[N - 1]; }, 3);
"#;

/// The array builtins over arrays past the dense cap. They run the same
/// native code in every tier and walk a million indices each, so they run
/// once, in-process.
#[cfg(not(feature = "safe-sandbox"))]
const PROBES_VIRTUAL_BUILTINS: &str = r#"
// ---- past the dense cap, through the array builtins ----
function edges() { var a = new Array(N); a[0] = 'a'; a[N - 1] = 'z'; return a; }
check('fill-range', function () { var a = new Array(N); a.fill(7, 5, 10); return [a[4], a[5], a[9], a[10], a.length]; }, [undefined, 7, 7, undefined, N]);
check('fill-tail', function () { var a = new Array(N); a.fill(7, N - 3); return [a[N - 4], a[N - 3], a[N - 1], keyCount(a)]; }, [undefined, 7, 7, 3]);
check('fill-length-set', function () { var c = []; c.length = N; c.fill(3); return [c[0], c[N - 1], c.length]; }, [3, 3, N]);
check('fill-overlay', function () { var a = edges(); a.fill(2); return [a[0], a[N - 2], a[N - 1]]; }, [2, 2, 2]);
check('fill-huge-refuses', function () { return rangeError(function () { new Array(4294967295).fill(0); }); }, true);
check('fill-huge-tail', function () { var a = new Array(4294967295); a.fill(1, 4294967292); return [a[4294967293], a[4294967294], a[4294967291], a.length]; }, [1, 1, undefined, 4294967295]);
check('at', function () { var a = edges(); return [a.at(-1), a.at(0), a.at(N), a.at(-N)]; }, ['z', 'a', undefined, 'a']);
check('slice', function () { var a = edges(); return [a.slice(N - 2), a.slice().length, a.slice(-1)[0], a.slice(1, 3).length]; }, [[undefined, 'z'], N, 'z', 2]);
check('join', function () { var a = edges(); return [a.join().length, a.join('').length, String(a).length, a.toLocaleString().length]; }, [N + 1, 2, N + 1, N + 1]);
check('concat', function () { var a = edges(); var r = a.concat(['t']); return [r.length, r[N - 1], r[N], 1 in r]; }, [N + 1, 'z', 't', false]);
check('find', function () { var a = edges(); return [a.find(function (x) { return x === 'z'; }), a.findLastIndex(function (x) { return x === 'z'; })]; }, ['z', N - 1]);
check('with', function () { var r = edges().with(N - 1, 'w'); return [r.length, r[N - 1], r[0]]; }, [N, 'w', 'a']);
check('toReversed', function () { var r = edges().toReversed(); return [r.length, r[0], r[N - 1]]; }, [N, 'z', 'a']);
check('toSpliced', function () { var r = edges().toSpliced(1, 1); return [r.length, r[N - 2], r[0]]; }, [N - 1, 'z', 'a']);
check('toSorted', function () { var r = edges().toSorted(); return [r.length, r[0], r[1], r[2]]; }, [N, 'a', 'z', undefined]);
check('sort', function () { var a = edges(); a.sort(); return [a.length, a[0], a[1], 2 in a]; }, [N, 'a', 'z', false]);
check('sort-huge', function () { var a = []; a.length = 4294967295; a[4294967294] = 'b'; a[7] = 'a'; a[3] = undefined; a.sort(); return [a[0], a[1], a[2], 2 in a, 3 in a, 7 in a, 4294967294 in a, a.length]; }, ['a', 'b', undefined, true, false, false, false, 4294967295]);
check('flat', function () { var r = edges().flat(); return [r.length, r[0], r[1]]; }, [2, 'a', 'z']);
check('flatMap', function () { var r = edges().flatMap(function (x) { return [x]; }); return [r.length, r[1]]; }, [2, 'z']);
check('copyWithin', function () { var a = edges(); a.copyWithin(1, N - 1); return [a[1], a.length]; }, ['z', N]);
check('reverse', function () { var a = edges(); a.reverse(); return [a[0], a[N - 1], a.length]; }, ['z', 'a', N]);
check('splice', function () { var a = edges(); var r = a.splice(0); return [r.length, r[N - 1], a.length]; }, [N, 'z', 0]);
check('search', function () { var a = edges(); return [a.indexOf('z'), a.lastIndexOf('a'), a.includes('z'), a.includes(undefined)]; }, [N - 1, 0, true, true]);
check('search-huge', function () { var a = []; a.length = 4294967295; a[4294967294] = 'x'; a[7] = 'y'; return [a.includes('x'), a.indexOf('x'), a.lastIndexOf('y'), a.includes(undefined), a.indexOf(undefined)]; }, [true, 4294967294, 7, true, -1]);
check('iterators', function () { var a = edges(); return [[...a.values()].length, [...a.keys()][N - 1]]; }, [N, N - 1]);
check('sparse-index', function () { var a = []; a[N + 5] = 'v'; a[3] = 'w'; return [a.at(-1), a.slice(-1)[0], [...a].length, a.concat([1]).length, a.join('').length]; }, ['v', 'v', N + 6, N + 7, 2]);
check('length-then-write', function () { var f = [1, 2, 3]; f.length = N + 1; f[N] = 9; return [f.at(-1), f.slice(-2), [...f].length]; }, [9, [undefined, 9], N + 1]);
check('reduce-map', function () { var a = new Array(N).fill(1); return [a.reduce(function (x, y) { return x + y; }, 0), a.map(function (x) { return x * 2; })[N - 1]]; }, [N, 2]);
check('arraylike-join', function () { return Array.prototype.join.call({ length: N + 2 }).length; }, N + 1);
check('arraylike-join-tail', function () { var o = { length: N + 2, 0: 'a' }; o[N + 1] = 'z'; var s = Array.prototype.join.call(o, ''); return [s.length, s[1]]; }, [2, 'z']);
check('arraylike-toSorted', function () { return Array.prototype.toSorted.call({ length: N + 2, 0: 'b', 1: 'a' }).length; }, N + 2);
check('eager-drains', function () {
  var a = []; for (var i = 0; i < N + 10; i++) a.push(i);
  function* g() { for (var j = 0; j < N + 10; j++) yield j; }
  return [Array.from(a).length, [...g()].length];
}, [N + 10, N + 10]);
check('huge-refuses', function () { return rangeError(function () { [...new Array(4294967295)]; }); }, true);
"#;

/// Everything else: small arrays, run in every mode including GC stress.
#[cfg(not(feature = "safe-sandbox"))]
const PROBES_SEMANTICS: &str = r#"
// ---- join keeps surrogate halves ----
var emoji = 'hi \u{1F600}!';
check('split-join', function () { return emoji.split('').join('') === emoji; }, true);
check('split-reverse-join', function () { return units('a\u{1F600}'.split('').reverse().join('')); }, 'de00 d83d 61');
check('pair-join', function () { return ['\uD83D', '\uDE00'].join('') === '\u{1F600}'; }, true);
check('string-of-array', function () { return units(String(['\uD800'])) + '|' + units(`${['\uDC00']}`) + '|' + units('' + ['\uD800', 'x']); }, 'd800|dc00|d800 2c 78');
check('arraylike-surrogates', function () { return units(Array.prototype.join.call({ length: 2, 0: '\uD83D', 1: '\uDE00' }, '')); }, 'd83d de00');
check('separator', function () { return units(['a', 'b'].join('\uD83D')) + '|' + units([1, 2].join({ toString: function () { return '\uDC00'; } })); }, '61 d83d 62|31 dc00 32');
check('nested', function () { return units([['\uD83D'], 'x'].join()); }, 'd83d 2c 78');
check('object-element', function () { return units([{ toString: function () { return '\uD800'; } }, 1].join('')); }, 'd800 31');
check('toLocaleString', function () { return units(['\uD800', { toLocaleString: function () { return '\uDC00'; } }].toLocaleString()); }, 'd800 2c dc00');
check('toLocaleString-undefined', function () { return [1, { toLocaleString: function () { return undefined; } }, null].toLocaleString(); }, '1,undefined,');
check('join-values', function () { return [1, 2.5, -0, NaN, true, null, undefined, 10n, 'x'].join('|'); }, '1|2.5|0|NaN|true|||10|x');
check('join-cycle', function () { var a = [1]; a.push(a); return a.join('-'); }, '1-');
check('join-symbol', function () { try { [Symbol('x')].join(); return 'joined'; } catch (e) { return e instanceof TypeError; } }, true);
check('join-hot', function () { var c = 0; for (var i = 0; i < 20000; i++) if (['\uD83D', '\uDE00'].join('') === '\u{1F600}') c++; return c; }, 20000);
check('typed-join', function () { return [units(new Uint8Array([1, 2]).join('\uD800')), units(new Float64Array([1.5]).join('\uD800'))]; }, ['31 d800 32', '31 2e 35']);

// ---- the other string builders keep lone surrogates too ----
check('String-as-callback', function () { return [units(['\uD800'].map(String)[0]), units([{ toString: function () { return '\uDC00'; } }].map(String)[0]), units(String.call(null, '\uD800'))]; }, ['d800', 'dc00', 'd800']);
check('symbol-description', function () { return [units(Symbol('\uD800').toString()), units(String(Symbol('\uDC00'))), units([Symbol('\uD800')].map(String)[0])]; }, ['53 79 6d 62 6f 6c 28 d800 29', '53 79 6d 62 6f 6c 28 dc00 29', '53 79 6d 62 6f 6c 28 d800 29']);
check('error-toString', function () { var e = new Error('\uD800'); e.name = 'E\uDC00'; var n = new Error('x'); n.name = ''; n.message = '\uDC00'; return [units(e.toString()), units(String(new TypeError('\uD800'))), units(n.toString())]; }, ['45 dc00 3a 20 d800', '54 79 70 65 45 72 72 6f 72 3a 20 d800', 'dc00']);
check('String.raw', function () { return [units(String.raw`a${'\uD800'}b`), String.raw({ raw: ['\uD83D', '\uDE00'] }, '') === '\u{1F600}', units(String.raw({ raw: ['x', '\uDC00'] }, '\uD800'))]; }, ['61 d800 62', true, '78 d800 dc00']);
check('escape', function () { return [escape('\uD800a\u{1F600}'), escape('\uDFFF')]; }, ['%uD800a%uD83D%uDE00', '%uDFFF']);

// ---- default sort compares UTF-16 code units ----
function cps(arr) { return arr.map(function (s) { return s === undefined ? 'u' : units(s); }).join(','); }
check('sort-astral', function () { return cps(['｡', '\u{1F600}'].sort()); }, 'd83d de00,ff61');
check('sort-private-use', function () { return cps(['', '\u{10000}'].sort()); }, 'd800 dc00,e000');
check('sort-lone', function () { return cps(['\uDC00', '\uD800'].sort()) + '|' + cps(['a\uDC00', 'a\uD800'].sort()); }, 'd800,dc00|61 d800,61 dc00');
check('toSorted-mixed', function () { return cps(['｡', '\u{1F600}', '\uD800', undefined, 'b'].toSorted()); }, '62,d800,d83d de00,ff61,u');
check('sort-matches-less-than', function () { var a = ['￿', 'a\u{1F600}', '퟿', '', 'a￿', '\u{10000}', 'ab']; var b = a.slice().sort(function (x, y) { return x < y ? -1 : x > y ? 1 : 0; }); return cps(a.sort()) === cps(b); }, true);
check('sort-plain', function () { return [10, 9, 1, 'b', 'a', undefined, 'B'].sort(); }, [1, 10, 9, 'B', 'a', 'b', undefined]);
check('sort-objects', function () { return cps([{ toString: function () { return '\uDC00'; } }, { toString: function () { return '\uD800'; } }].sort().map(String)); }, 'd800,dc00');

// ---- Object.prototype.toString reads [[ErrorData]] ----
var tag = function (v) { return Object.prototype.toString.call(v); };
check('error-tag', function () {
  class V extends Error { constructor() { super('x'); this.name = 'V'; } }
  class W extends TypeError { }
  var renamed = new Error('x'); renamed.name = 'ValidationError';
  var caught; try { null.f(); } catch (e) { e.name = 'Wrapped'; caught = e; }
  var unnamed = new Error(); delete unnamed.name;
  return [tag(new V()), tag(new W('y')), tag(renamed), tag(caught), tag(unnamed), tag(new AggregateError([]))];
}, ['[object Error]', '[object Error]', '[object Error]', '[object Error]', '[object Error]', '[object Error]']);
check('non-error-tag', function () {
  return [tag({ name: 'Error' }), tag(JSON.parse('{"name":"TypeError","message":"x"}')), String({ name: 'TypeError', message: 'hi' }), tag(Error), tag(TypeError), tag(Error.prototype), tag(Object.create(Error.prototype))];
}, ['[object Object]', '[object Object]', '[object Object]', '[object Function]', '[object Function]', '[object Object]', '[object Object]']);
check('error-tag-hot', function () { class V extends Error { constructor() { super('q'); this.name = 'V'; } } var c = 0; for (var i = 0; i < 20000; i++) if (tag(new V()) === '[object Error]') c++; return c; }, 20000);

// ---- toSpliced coerces its arguments ----
check('toSpliced-coerce', function () { return [[1, 2].toSpliced('1', 0, 3), [1, 2, 3].toSpliced(0, '2'), [1, 2].toSpliced(true, 0, 3), Array.prototype.toSpliced.call([1, 2], '1', 0, 3), [1, 2, 3].toSpliced({ valueOf: function () { return 1; } }, 1), [1, 2, 3].toSpliced(undefined, 1)]; }, [[1, 3, 2], [3], [1, 3, 2], [1, 3, 2], [1, 3], [2, 3]]);
check('toSpliced-bigint', function () { try { [1, 2].toSpliced(1n, 0, 3); return 'spliced'; } catch (e) { return e instanceof TypeError; } }, true);
check('toSpliced-numbers', function () { return [[1, 2, 3].toSpliced(), [1, 2, 3].toSpliced(1), [1, 2, 3].toSpliced(-1, 1, 'x', 'y'), [, 2].toSpliced(0, 0), [1, 2, 3].toSpliced(NaN, Infinity), [1, 2, 3].toSpliced(-Infinity, 1), [1, 2, 3].toSpliced(1.7, 0.9, 'x')]; }, [[1, 2, 3], [1], [1, 2, 'x', 'y'], [undefined, 2], [], [2, 3], [1, 'x', 2, 3]]);

// ---- the length read at entry bounds the work ----
check('includes-shrunk', function () { var a = [1, 2, 3]; return a.includes(undefined, { valueOf: function () { a.length = 1; return 0; } }); }, true);
check('slice-shrunk', function () { var a = [1, 2, 3, 4]; var r = a.slice({ valueOf: function () { a.length = 1; return 0; } }); return [r.length, 0 in r, 1 in r]; }, [4, true, false]);
check('toSpliced-grown', function () { var b = [1, 2, 3]; return b.toSpliced({ valueOf: function () { b.push(4); return 1; } }, 1); }, [1, 3]);
check('sort-appended', function () { var a = [3, 1, 2]; a.sort(function (x, y) { if (a.length < 6) a.push(9); return x - y; }); return [a.length, a.slice(0, 3)]; }, [6, [1, 2, 3]]);

// ---- holes: flat skips them, at reads through the prototype ----
check('flat-holes', function () { var r = [1, , 3].flat(); var n = [[1, , 3]].flat(); var d = [1, [2, , [3]]].flat(); return [r.length, n.length, 1 in n, d.length]; }, [2, 2, true, 3]);
check('flat-proxy', function () { return [new Proxy([1, 2], {})].flat(); }, [1, 2]);
check('flat-depth', function () { return [[1, [2, [3, [4]]]].flat(Infinity), [1, [2]].flat(0).length, [1, [2, [3]]].flat(-1).length]; }, [[1, 2, 3, 4], 2, 2]);
check('flat-species', function () { class A extends Array { } var r = A.from([1, [2, 3]]).flat(); return [r instanceof A, r.length]; }, [true, 3]);
check('flat-inherited', function () { Array.prototype[1] = 'P'; try { return [[1, , 3, , 5].at(1), [1, , 3, , 5].flat()]; } finally { delete Array.prototype[1]; } }, ['P', [1, 'P', 3, 5]]);
check('at-accessor', function () { var a = [1, 2]; Object.defineProperty(a, 1, { get: function () { return 'g'; } }); return [a.at(1), a.at(-1)]; }, ['g', 'g']);

// ---- concat copies only present indices of a spreadable non-array ----
check('concat-proxy-holes', function () { var r = [].concat(new Proxy([1, , 3], {})); return [r.length, 1 in r, Object.keys(r)]; }, [3, false, ['0', '2']]);
check('concat-arraylike-holes', function () { var al = { length: 3, 0: 'a', 2: 'c' }; al[Symbol.isConcatSpreadable] = true; Array.prototype[1] = 'P'; try { var r = [].concat(al); return [Object.hasOwn(r, 1), r[1], r.length]; } finally { delete Array.prototype[1]; } }, [false, 'P', 3]);
check('concat-has-trap', function () { var log = []; var p = new Proxy([1, , 3], { get: function (t, k) { log.push('get:' + String(k)); return t[k]; }, has: function (t, k) { log.push('has:' + String(k)); return k in t; } }); [].concat(p); return log.join(','); }, 'get:Symbol(Symbol.isConcatSpreadable),get:length,has:0,get:0,has:1,has:2,get:2');
check('concat-repeated', function () { var a = [1, 2, 3]; var b = [a, a, a, a]; var r = Array.prototype.concat.apply([], b); return [r.length, r[11], [0].concat(a, [4], 5).length]; }, [12, 3, 6]);

// ---- buffer methods are the ones a Get finds ----
check('arraybuffer-no-grow', function () { var ab = new ArrayBuffer(8, { maxByteLength: 64 }); var r; try { ab.grow(32); r = 'grew'; } catch (e) { r = e instanceof TypeError; } return [typeof ab.grow, r, ab.byteLength]; }, ['undefined', true, 8]);
check('arraybuffer-grow-call', function () { var ab = new ArrayBuffer(8, { maxByteLength: 64 }); try { SharedArrayBuffer.prototype.grow.call(ab, 32); return 'grew'; } catch (e) { return [e instanceof TypeError, ab.byteLength]; } }, [true, 8]);
check('shared-no-resize', function () { var s = new SharedArrayBuffer(8, { maxByteLength: 64 }); var r = []; ['resize', 'transfer', 'transferToFixedLength'].forEach(function (m) { try { s[m](16); r.push('ran'); } catch (e) { r.push(e instanceof TypeError); } }); s.grow(32); r.push(s.byteLength); return r; }, [true, true, true, 32]);
check('deleted-method', function () { var saved = ArrayBuffer.prototype.resize; delete ArrayBuffer.prototype.resize; var ab = new ArrayBuffer(8, { maxByteLength: 64 }); try { ab.resize(16); return 'resized'; } catch (e) { return [e instanceof TypeError, ab.byteLength]; } finally { ArrayBuffer.prototype.resize = saved; } }, [true, 8]);
check('replaced-method', function () { var saved = ArrayBuffer.prototype.slice; ArrayBuffer.prototype.slice = function () { return 'replaced'; }; try { return new ArrayBuffer(8).slice(); } finally { ArrayBuffer.prototype.slice = saved; } }, 'replaced');
check('null-prototype', function () { var ab = new ArrayBuffer(8); Object.setPrototypeOf(ab, null); return typeof ab.slice; }, 'undefined');
check('buffer-methods', function () { var ab = new ArrayBuffer(8, { maxByteLength: 64 }); ab.resize(16); var moved = ab.transfer(32); var fixed = moved.transferToFixedLength(); var part = fixed.slice(2, 4); var imm = part.transfer(); return [ab.detached, moved.detached, fixed.byteLength, fixed.resizable, part.detached, imm.byteLength, new SharedArrayBuffer(4).slice(1).byteLength]; }, [true, true, 32, false, true, 2, 3]);
check('buffer-hot', function () { var ab = new ArrayBuffer(16); var n = 0; for (var i = 0; i < 20000; i++) n += ab.slice(0, 4).byteLength; var s = new SharedArrayBuffer(1, { maxByteLength: 4096 }); for (var j = 2; j <= 4096; j++) s.grow(j); return [n, s.byteLength]; }, [80000, 4096]);

"#;

/// `MAX_DENSE_ARRAY_LEN` of the ordinary profile. The probes are pinned to it:
/// the hardened profile caps both the dense store and the materialization at
/// 2^22, so its "past the cap" shapes are a different test (and it has no JIT
/// tiers to compare).
#[cfg(not(feature = "safe-sandbox"))]
const DENSE_CAP: usize = 1 << 20;

/// A probe program over `bodies`, with `N` bound to one past the dense cap,
/// and the number of `check` lines it must print.
#[cfg(not(feature = "safe-sandbox"))]
fn probe_source(bodies: &[&str]) -> (String, usize) {
    let body = bodies.concat();
    let checks = body.matches("\ncheck(").count();
    let source = format!(
        "var N = {};\n{HARNESS}{body}console.log(lines.join('\\n'));\n",
        DENSE_CAP + 1
    );
    (source, checks)
}

/// Every probe line of one run passed, and none is missing.
#[cfg(not(feature = "safe-sandbox"))]
fn assert_probes_pass(lines: &[String], checks: usize) {
    let text = lines.join("\n");
    let failures: Vec<&str> = text.lines().filter(|l| !l.ends_with("=ok")).collect();
    assert!(
        failures.is_empty(),
        "probe failures:\n{}",
        failures.join("\n")
    );
    assert_eq!(text.lines().count(), checks, "probe output truncated:\n{text}");
}

/// The array builtins past the dense cap, once, in this process.
#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn builtins_use_the_length_of_arrays_past_the_dense_cap() {
    if std::env::var_os("ZIPP_ARRAYS_PROBE_CHILD").is_some() {
        return;
    }
    let (source, checks) = probe_source(&[PROBES_VIRTUAL_BUILTINS]);
    assert_probes_pass(&run_ok(&source), checks);
}

/// The per-mode probes, in whatever mode the environment selects. A no-op
/// unless the modes test below spawned this process for it.
#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn arrays_probe_child() {
    let Some(set) = std::env::var_os("ZIPP_ARRAYS_PROBE_CHILD") else {
        return;
    };
    let (source, checks) = if set == "semantics" {
        probe_source(&[PROBES_SEMANTICS])
    } else {
        probe_source(&[PROBES_VIRTUAL_TIERS, PROBES_SEMANTICS])
    };
    assert_probes_pass(&run_ok(&source), checks);
}

#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn arrays_probe_every_mode() {
    if std::env::var_os("ZIPP_ARRAYS_PROBE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, set, env) in [
        ("default", "all", None),
        ("interpreter", "all", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", "all", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        // Collecting at every allocation over million-element arrays takes
        // minutes; the small shapes carry the rooting questions.
        ("gc-stress", "semantics", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "arrays_probe_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env("ZIPP_ARRAYS_PROBE_CHILD", set);
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "arrays probes / {mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A buffer the host resolved with `typed_array_region` is pinned: the host
/// holds a raw address into it. Neither a resizable ArrayBuffer nor a
/// growable SharedArrayBuffer (where the profile has one) may then be grown,
/// resized, transferred or detached by the guest — growing a local buffer
/// past its capacity moved its bytes and freed the old ones under the host's
/// region. The region the host resolves afterwards is the one it was given.
#[test]
fn host_pinned_buffers_are_never_grown_resized_or_detached() {
    use std::sync::{Arc, Mutex};
    use zipp_vm::embed::{self, JsValue};

    let mut st = embed::compile_script(
        r#"
        var shared = typeof SharedArrayBuffer === 'function';
        var AB = new ArrayBuffer(64, { maxByteLength: 1 << 20 });
        var A = new Uint8Array(AB, 0, 64);
        var SAB = shared ? new SharedArrayBuffer(64, { maxByteLength: 1 << 20 }) : null;
        var S = shared ? new Uint8Array(SAB, 0, 64) : null;
        function pin() { return __zippHostCall('pin', String(shared)); }
        function attempt(label, f) {
            try { f(); return label + ':completed'; }
            catch (e) { return label + ':' + e.name + ':' + e.message; }
        }
        function mutate() {
            var lines = [
                attempt('grow', function () { AB.grow(1 << 20); }),
                attempt('resize', function () { AB.resize(1 << 20); }),
                attempt('transfer', function () { AB.transfer(); }),
                attempt('transferToFixedLength', function () { AB.transferToFixedLength(); }),
                attempt('transferToImmutable', function () { AB.transferToImmutable(); })
            ];
            if (shared) {
                lines.push(attempt('shared-grow', function () { SAB.grow(1 << 20); }));
                lines.push(attempt('grow-call', function () { SharedArrayBuffer.prototype.grow.call(AB, 1 << 20); }));
            }
            lines.push('state:' + AB.byteLength + ',' + AB.detached + ',' + (shared ? SAB.byteLength : 64));
            return lines.join('\n');
        }
    "#,
    )
    .expect("compiles");
    st.run_init().expect("runs");
    let regions: Arc<Mutex<Vec<(usize, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&regions);
    st.set_host_call_ctx(Box::new(move |ctx, kind, args| {
        assert_eq!(kind, "pin");
        let mut names = vec!["A"];
        if args.first().map(String::as_str) == Some("true") {
            names.push("S");
        }
        for name in names {
            let (at, len, _) = ctx.typed_array_region(name)?;
            seen.lock().unwrap().push((at, len));
        }
        Ok(String::new())
    }));
    st.call_global("pin", &[]).expect("pins");
    let report = match st.call_global("mutate", &[]).expect("mutate runs") {
        JsValue::String(s) => s,
        other => panic!("unexpected {other:?}"),
    };
    let lines: Vec<&str> = report.lines().collect();
    let shared = lines.len() == 8;
    // A plain ArrayBuffer has no `grow` at all, and calling SharedArrayBuffer's
    // on it is a brand error. Every other attempt meets the pin.
    assert!(lines[0].starts_with("grow:TypeError"), "{report}");
    let pinned = if shared { &lines[1..6] } else { &lines[1..5] };
    for line in pinned {
        assert!(
            line.contains(":TypeError:") && line.contains("pinned"),
            "{line} (full report:\n{report})"
        );
    }
    if shared {
        assert!(lines[6].starts_with("grow-call:TypeError"), "{report}");
    }
    assert_eq!(*lines.last().unwrap(), "state:64,false,64", "{report}");
    // Re-resolving finds the same bytes at the same address.
    st.call_global("pin", &[]).expect("re-pins");
    let regions = regions.lock().unwrap();
    let per_pin = regions.len() / 2;
    assert_eq!(per_pin, if shared { 2 } else { 1 });
    assert_eq!(regions[..per_pin], regions[per_pin..], "a pinned buffer moved");
}

/// The pin is checked where the buffer is mutated, after the newLength
/// coercion: a `valueOf` that pins the buffer (through a host call) meets
/// the pin. Checked only before the coercion, `resize` went ahead and moved
/// the bytes under the region the host had just resolved, and the transfers
/// detached the freshly pinned buffer.
#[test]
fn a_pin_taken_during_argument_coercion_is_honoured() {
    use std::sync::{Arc, Mutex};
    use zipp_vm::embed::{self, JsValue};

    let mut st = embed::compile_script(
        r#"
        var methods = ['resize', 'transfer', 'transferToFixedLength', 'transferToImmutable'];
        var bufs = {};
        methods.forEach(function (m) { bufs[m] = new ArrayBuffer(64, { maxByteLength: 1 << 20 }); });
        var V_resize = new Uint8Array(bufs.resize, 0, 64);
        var V_transfer = new Uint8Array(bufs.transfer, 0, 64);
        var V_transferToFixedLength = new Uint8Array(bufs.transferToFixedLength, 0, 64);
        var V_transferToImmutable = new Uint8Array(bufs.transferToImmutable, 0, 64);
        function attack(m) {
            var len = { valueOf: function () {
                __zippHostCall('pin', 'V_' + m);
                return m === 'resize' ? (1 << 20) : 64;
            } };
            try { bufs[m][m](len); return m + ':completed'; }
            catch (e) { return m + ':' + e.name + ':' + e.message; }
        }
        function run() {
            return methods.map(function (m) {
                return attack(m) + '|' + bufs[m].byteLength + ',' + bufs[m].detached;
            }).join('\n');
        }
        function repin() { methods.forEach(function (m) { __zippHostCall('pin', 'V_' + m); }); }
    "#,
    )
    .expect("compiles");
    st.run_init().expect("runs");
    let regions: Arc<Mutex<Vec<(usize, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&regions);
    st.set_host_call_ctx(Box::new(move |ctx, kind, args| {
        assert_eq!(kind, "pin");
        let (at, len, _) = ctx.typed_array_region(&args[0])?;
        seen.lock().unwrap().push((at, len));
        Ok(String::new())
    }));
    let report = match st.call_global("run", &[]).expect("run") {
        JsValue::String(s) => s,
        other => panic!("unexpected {other:?}"),
    };
    for line in report.lines() {
        let (outcome, state) = line.split_once('|').expect("two parts");
        assert!(
            outcome.contains(":TypeError:") && outcome.contains("pinned"),
            "{line} (full report:\n{report})"
        );
        assert_eq!(state, "64,false", "{line}");
    }
    st.call_global("repin", &[]).expect("re-pins");
    let regions = regions.lock().unwrap();
    assert_eq!(regions.len(), 8);
    assert_eq!(regions[..4], regions[4..], "a pinned buffer moved");
}

/// `concat` and `flat` over many references to one large array make a result
/// far larger than their input. Built with no preflight, the result was
/// committed whole and only noticed at the next bytecode poll: 4 GB under a
/// 16 MB ceiling natively, and a trap that poisoned the WebAssembly engine.
/// Each now admits its store before growing it.
#[cfg(feature = "instrument")]
mod heap_ceiling {
    use zipp_vm::embed::{self, ScriptState};

    const CEILING: usize = 64 << 20;

    fn slot(state: &ScriptState, name: &str) -> u32 {
        state
            .symbols()
            .into_iter()
            .find(|symbol| symbol.name == name)
            .unwrap_or_else(|| panic!("missing global function {name}"))
            .index
    }

    fn assert_convicted_before_building(body: &str) {
        let source = format!(
            "var a = new Array(1 << 20).fill(0); var b = new Array(64).fill(a);\n\
             function run() {{ var r = {body}; return r.length; }}\n"
        );
        let mut state = embed::compile_script(&source).expect("script compiles");
        state.disable_vm_jit();
        state.run_init().expect("script initializes");
        state.set_limits(u64::MAX, None);
        state.set_heap_limit(CEILING);
        let run = slot(&state, "run");
        let result = state.call_slot(run, &[]);
        let peak = state.heap_bytes();
        match result {
            Err(message) => assert!(
                message.contains("memory budget"),
                "{body}: expected the heap ceiling to convict, got: {message}"
            ),
            Ok(value) => panic!("{body}: completed with {value:?} at {peak} bytes"),
        }
        // The 512 MB result was never committed.
        assert!(
            peak <= CEILING + CEILING / 2,
            "{body}: the result ran away to {peak} bytes under a {CEILING}-byte ceiling"
        );
    }

    #[test]
    fn concat_of_repeated_references_meets_the_ceiling() {
        assert_convicted_before_building("Array.prototype.concat.apply([], b)");
    }

    #[test]
    fn flat_of_repeated_references_meets_the_ceiling() {
        assert_convicted_before_building("b.flat()");
    }
}
