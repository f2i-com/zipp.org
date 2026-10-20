//! `JSON.parse` allocates its tree old only for a large source (a document
//! a program keeps); a small message parsed in a loop is young. Messages
//! kept across later collections, a large document, a reviver and the
//! reviver's source context must all read back exactly. Expected output
//! is node's.

#[test]
fn json_parse_results_read_back_whether_born_young_or_old() {
    let src = r###"const out = [];
const kept = [];
let sum = 0;
for (let i = 0; i < 400; i++) {
  const msg = JSON.parse('{"id":' + i + ',"tags":["a","b' + (i % 7) + '"],"n":{"x":' + i * 3 + ',"s":"v' + i + '"}}');
  sum += msg.n.x + msg.tags[1].length;
  if (i % 97 === 0) kept.push(msg);
}
const bigSrc = JSON.stringify(Array.from({ length: 2000 }, (_, i) => ({ k: "key" + i, v: [i, i * 2, { d: "x".repeat(i % 9) }] })));
const big = JSON.parse(bigSrc);
const garbage = [];
for (let i = 0; i < 3000; i++) garbage.push({ g: i, s: "tmp" + i });
garbage.length = 0;
for (let i = 0; i < 200; i++) JSON.parse('[' + i + ',{"q":"' + i + '"}]');
out.push(sum + " " + kept.map((m) => m.id + ":" + m.n.s + ":" + m.tags.join("")).join(","));
out.push(big.length + " " + big[1999].k + " " + big[1234].v[2].d.length + " " + big[7].v[1]);
const rev = JSON.parse('{"a":[1,2,{"b":3}],"c":"d"}', function (k, v) { return typeof v === "number" ? v * 10 : v; });
out.push(JSON.stringify(rev));
const src = [];
JSON.parse('{"p":[10,"t"]}', function (k, v, ctx) { if (ctx && "source" in ctx) src.push(k + "=" + ctx.source); return v; });
out.push(src.join(" "));
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"240200 0:v0:ab0,97:v97:ab6,194:v194:ab5,291:v291:ab4,388:v388:ab3
2000 key1999 1 14
{"a":[10,20,{"b":30}],"c":"d"}
0=10 1="t""###);
}
