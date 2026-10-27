//! A method call on a TypedArray runs the method a real Get finds: a
//! subclass's override of any `%TypedArray%.prototype` method (Node's
//! `Buffer extends Uint8Array` overrides `indexOf`, `lastIndexOf`,
//! `includes`, `slice`, `toString`), a patched `Uint8Array.prototype`
//! method, or an own shadow, never the built-in it replaces. Each method
//! is overridden in turn and called, at the default thresholds and as hot
//! loop code.

const PROGRAM: &str = r#"
class Buf extends Uint8Array {}
const base = Object.getPrototypeOf(Uint8Array.prototype);
const names = Object.getOwnPropertyNames(base).filter((n) => {
  const d = Object.getOwnPropertyDescriptor(base, n);
  return typeof d.value === "function" && n !== "constructor";
});
const missed = [];
for (const n of names) {
  Buf.prototype[n] = function () { return "own"; };
  const b = new Buf(4);
  let hits = 0;
  for (let i = 0; i < 300; i++) {
    let r;
    try { r = b[n](1, 2); } catch (e) { r = "threw"; }
    if (r === "own") hits++;
  }
  if (hits !== 300) missed.push(n);
  delete Buf.prototype[n];
}
Uint8Array.prototype.fill = function () { return "patched"; };
const patched = new Uint8Array(2).fill(1);
delete Uint8Array.prototype.fill;
const restored = new Uint8Array(2).fill(1)[1];
const shadow = new Uint8Array(2);
shadow.indexOf = () => "shadow";
console.log(names.length > 25, "missed:" + missed.join(","), patched, restored, shadow.indexOf(0), new Buf(3).indexOf(0));
"#;

#[test]
fn typed_array_method_overrides_are_called() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, ["true missed: patched 1 shadow 0"]);
}
