//! Regex literals evaluated hot. Each evaluation is a new object, as the
//! spec requires, though the objects of one literal share its compiled
//! program, source and flags (`Vm::new_regexp_literal`): `lastIndex`
//! starts at 0 on each and moves independently, a property set on one is
//! not on the next, global, sticky, `u` and lone-surrogate patterns behave
//! as written, and `match`/`split`/`replace` take them as arguments. The
//! expected line is Node 24's.

#[test]
fn every_evaluation_of_a_regex_literal_is_a_new_object() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        [
            r#"5000,1443,false,0,false,a(b+)c,g,/a(b+)c/g,true,false,0,true,2,false,true,\uD800|x,2000,true,true,300,d,a+b+c,ayby"#
        ]
    );
}

const PROGRAM: &str = r####"
// Regex literals evaluated hot: each evaluation is a new object.
const out = [];
function make() { return /a(b+)c/g; }
function sticky() { return /x/y; }
function test(s) { return /[a-z_0-9]/.test(s); }
let hits = 0, prev = null, distinct = 0;
for (let i = 0; i < 5000; i++) {
  const r = make();
  if (r !== prev) distinct++;
  prev = r;
  if (r.lastIndex !== 0) out.push("dirty lastIndex " + i);
  r.exec("abbbc abc");
  if (r.lastIndex !== 5) out.push("bad lastIndex " + r.lastIndex);
  if (test(String.fromCharCode(i % 128))) hits++;
}
out.push(distinct, hits);
const a = make(), b = make();
a.lastIndex = 3; a.foo = 1;
out.push(a === b, b.lastIndex, "foo" in b, a.source, a.flags, String(b), a.global, b.sticky);
const s1 = sticky(); s1.lastIndex = 1;
out.push(sticky().lastIndex, s1.test("ax"), s1.lastIndex, sticky().test("ax"));
function lone() { return /\uD800|x/; }
for (let i = 0; i < 20; i++) lone();
out.push(lone().test("\uD800"), lone().source);
function uni() { return /\p{L}+/u; }
let u = 0;
for (let i = 0; i < 2000; i++) if (uni().test("héllo" + i)) u++;
out.push(u, uni().unicode, Object.getPrototypeOf(uni()) === RegExp.prototype);
const words = [];
for (let i = 0; i < 300; i++) { const m = "id_" + i + " x".match(/[a-z_]+(\d+)/); words.push(m[1]); }
out.push(words.length, words[299]);
out.push("a-b-c".split(/-/).join("+"), "aXbX".replace(/X/g, "y"));
console.log(out.join(","));
"####;
