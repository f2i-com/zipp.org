//! A string `split` allocates its result old only when it is large (a
//! document's lines); a smaller one, made in a loop, is young. Both kinds,
//! and fields kept from young results across later collections, must
//! read back exactly. Expected output is node's.

#[test]
fn split_results_read_back_whether_born_young_or_old() {
    let src = r###"const out = [];
const doc = Array.from({ length: 300 }, (_, i) => "row" + i + ";a" + i + ";b" + (i * 7) + ";" + "x".repeat(i % 5)).join("\n");
const lines = doc.split("\n");
const kept = [];
for (let r = 0; r < 3; r++) {
  for (let i = 0; i < lines.length; i++) {
    const f = lines[i].split(";");
    if (i % 50 === 0) kept.push(f);
    const g = f.join("|").split("|");
    if (g.length !== f.length || g[1] !== f[1]) out.push("mismatch " + i);
  }
}
const small = "a,b,c".split(",");
const one = "abc".split();
const chars = "héllo".split("");
const lim = "a;b;c;d".split(";", 2);
out.push(kept.length + " " + kept[3].join("/") + " " + kept[kept.length - 1][2]);
out.push(small.join("+") + " " + one[0] + " " + chars.join("_") + " " + lim.join(","));
const big = "w,".repeat(500).split(",");
out.push(big.length + " " + big[499] + "|" + big[500] + "|");
console.log(out.join("\n"));"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"18 row150/a150/b1050/ b1750
a+b+c abc h_é_l_l_o a,b
501 w||"###);
}
