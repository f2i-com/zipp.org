//! `ta.<method>` reads and the species proof of `subarray`/`slice`, both
//! memoized under prototype versions. Each case warms a loop on the pristine
//! chain, then changes one link (an own shadow, the kind prototype, an
//! in-place overwrite or accessor on %TypedArray%.prototype, a deleted method,
//! `setPrototypeOf` on the view or on the kind prototype, a subclass, the
//! `constructor` and `@@species` links) and reads again. The expected lines
//! are Node's.

const SOURCE: &str = r##"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const TAP = Object.getPrototypeOf(Uint8Array.prototype);
const TA = Object.getPrototypeOf(Uint8Array);
function warm(ta, n) {
  let h = 0;
  for (let i = 0; i < n; i++) {
    const v = ta.subarray(1, 5);
    h = (h + v.length + v[0] + ta.indexOf(3) + ta.slice(0, 2).length) | 0;
    if (ta.includes(9)) h++;
  }
  return h;
}
const base = () => new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
log("pristine", warm(base(), 3000));

// Own shadow on one view.
{ const a = base(); warm(a, 500); a.subarray = () => new Uint8Array([9, 9]); log("own", warm(a, 500), warm(base(), 500)); }

// Kind prototype shadow, then removed.
{ Uint8Array.prototype.indexOf = function () { return 100; };
  log("kind shadow", warm(base(), 500), warm(new Int8Array([1, 2, 3, 4, 5, 6]), 500));
  delete Uint8Array.prototype.indexOf; log("kind restored", warm(base(), 500)); }

// In-place overwrite of %TypedArray%.prototype.slice, then restored.
{ const orig = TAP.slice; TAP.slice = function () { return [7, 7, 7]; };
  log("overwrite", warm(base(), 500)); TAP.slice = orig; log("restored", warm(base(), 500)); }

// Accessor on %TypedArray%.prototype.
{ const orig = Object.getOwnPropertyDescriptor(TAP, "includes"); let calls = 0;
  Object.defineProperty(TAP, "includes", { get() { calls++; return () => true; }, configurable: true });
  log("accessor", warm(base(), 300), calls); Object.defineProperty(TAP, "includes", orig); log("accessor gone", warm(base(), 300)); }

// Deleted method.
{ const orig = TAP.includes; delete TAP.includes;
  try { warm(base(), 10); } catch (e) { log("deleted", e.constructor.name); }
  TAP.includes = orig; log("re-added", warm(base(), 300)); }

// setPrototypeOf on a view and on a kind prototype.
{ const a = base(); warm(a, 300); Object.setPrototypeOf(a, { subarray() { return new Uint8Array(1); }, indexOf() { return -5; }, slice() { return [1]; }, includes() { return false; } });
  log("view reproto", warm(a, 300));
  const p = Object.getPrototypeOf(Int16Array.prototype); const proxyP = Object.create(p, { indexOf: { value() { return 42; } } });
  Object.setPrototypeOf(Int16Array.prototype, proxyP); log("kind reproto", warm(new Int16Array([1, 2, 3, 4]), 300));
  Object.setPrototypeOf(Int16Array.prototype, p); log("kind back", warm(new Int16Array([1, 2, 3, 4]), 300)); }

// Subclass instances.
{ class B extends Uint8Array { subarray(s, e) { return super.subarray(s, e + 1); } indexOf() { return 1000; } }
  log("subclass", warm(new B([1, 2, 3, 4, 5, 6]), 300), warm(base(), 300)); }

// Species: constructor on the view, on the kind prototype, @@species on the constructor and on %TypedArray%.
function spec(ta, n) { let h = 0; for (let i = 0; i < n; i++) { const s = ta.subarray(0, 3), c = ta.slice(1, 4); h = (h + s.length * 10 + c.length + (s instanceof Uint8Array ? 100 : 0) + (c.constructor === Uint8Array ? 1000 : 0)) | 0; } return h; }
const t = (f) => { try { return f(); } catch (e) { return e.constructor.name; } };
log("spec pristine", t(() => spec(base(), 2000)));
{ const a = base(); spec(a, 200); a.constructor = Int8Array; log("spec own ctor", t(() => spec(a, 200)));
  a.constructor = Int16Array; log("spec own ctor wide", t(() => spec(a, 200))); }
{ const d = Object.getOwnPropertyDescriptor(Uint8Array.prototype, "constructor");
  Uint8Array.prototype.constructor = Uint8ClampedArray; log("spec kind ctor", t(() => spec(base(), 200)));
  Object.defineProperty(Uint8Array.prototype, "constructor", d); log("spec kind back", t(() => spec(base(), 200))); }
{ let n = 0; Object.defineProperty(Uint8Array, Symbol.species, { get() { n++; return n > 50 ? Int32Array : Int8Array; }, configurable: true });
  log("spec ctor species", t(() => spec(base(), 200)), n); delete Uint8Array[Symbol.species]; log("spec ctor gone", t(() => spec(base(), 200))); }
{ const d = Object.getOwnPropertyDescriptor(TA, Symbol.species); let n = 0;
  Object.defineProperty(TA, Symbol.species, { get() { n++; return this === Uint8Array ? Uint8ClampedArray : this; }, configurable: true });
  log("spec base species", t(() => spec(base(), 200)), n); Object.defineProperty(TA, Symbol.species, d); log("spec base back", t(() => spec(base(), 200))); }
{ const p = Object.getPrototypeOf(Uint8Array); Object.setPrototypeOf(Uint8Array, Object.create(p, { [Symbol.species]: { get() { return Int8Array; } } }));
  log("spec ctor reproto", t(() => spec(base(), 200))); Object.setPrototypeOf(Uint8Array, p); log("spec ctor back", t(() => spec(base(), 200))); }

// Relative indices: Ints, doubles, negatives, strings, valueOf, out of range.
{ const a = new Uint8Array([10, 20, 30, 40, 50]); const r = [];
  for (const [s, e] of [[1, 3], [-2, undefined], [1.7, 4.2], ["1", "3"], [{ valueOf() { return 2; } }, 9], [-99, 99], [NaN, -1], [3, 1], [2 ** 31, -(2 ** 31)]])
    r.push(Array.from(a.subarray(s, e)).join("") + "/" + Array.from(a.slice(s, e)).join(""));
  log("rel", r.join(" ")); }
console.log(out.join("\n"));
"##;

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn ta_method_reads_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    assert_eq!(got, node_output(SOURCE));
}

/// The same corpus with the interpreter only, eager JIT and GC stress, each
/// in a fresh process.
#[test]
fn ta_method_reads_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [("ZIPP_NOJIT", "1"), ("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")] {
        let out = std::process::Command::new(&exe)
            .args(["ta_method_reads_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}
