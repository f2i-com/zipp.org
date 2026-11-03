//! Non-ASCII string paths against Node: `normalize` in every form (ASCII
//! receivers come back as they are, short ones are normalized once, results
//! equal to the receiver are the receiver), `indexOf`/`lastIndexOf`/
//! `includes` from scattered and sequential positions in a long two-byte
//! string with astral pairs and lone surrogates (the position memo moves to
//! a match; marks answer far seeks), and the one-code-point strings that
//! iteration, `charAt`, `at`, spread and regex matches hand out from the
//! heap's cache, used as values, keys and accumulator pieces. The expected
//! lines are Node 24's.

const SOURCE: &str = r##"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const show = (v) => JSON.stringify(v);
let seed = 7;
function rnd(n) { seed = (Math.imul(seed, 1103515245) + 12345) >>> 0; return seed % n; }
function hash(s) { let h = 2166136261; for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619); return h >>> 0; }

// normalize: each form over ASCII, composed, decomposed, Hangul, compatibility
// characters, long runs of marks, astral text and lone surrogates.
const NORM = ["plain ascii", "", "café", "café", "Å", "Å", "Å", "각",
  "각", "ﬁ", "①", "ｶ", "Ǆ", "ệ", "x̧̖́̀", "a" + "̖́".repeat(20),
  "ΩΩ", "😀́", "lone \ud800 x \udc00", "abćdef", "ǺB"];
for (const s of NORM) {
  log("norm", show(s), ["NFC", "NFD", "NFKC", "NFKD"].map((f) => show(s.normalize(f))).join(" "), show(s.normalize()));
}
let normHash = 0;
for (let r = 0; r < 400; r++) {
  let s = "";
  for (let k = 0; k < 6; k++) s += NORM[rnd(NORM.length)];
  for (const f of ["NFC", "NFD", "NFKC", "NFKD"]) normHash = (normHash + hash(s.normalize(f))) >>> 0;
  if (s.normalize("NFD").normalize("NFC") !== s.normalize("NFC")) log("roundtrip", show(s));
}
log("normHash", normHash);
try { "x".normalize("NFX"); } catch (e) { log("bad form", e.constructor.name); }

// Searches in a long two-byte string.
const PIECES = ["日本", "😀", "ß", "Привет", "abc", "é", "一丁", "z0", "x", "\ud800", "\udc00", "ab😀c", "  "];
let big = "";
for (let i = 0; i < 6000; i++) big += PIECES[rnd(PIECES.length)] + (i % 7 === 0 ? "😀" : "é");
let found = 0, sum = 0;
for (const needle of ["日本", "😀", "ß", "Привет", "abc", "é", "z0", "\ud800", "\udc00", "c😀", "notthere"]) {
  let pos = big.indexOf(needle);
  while (pos >= 0) { found++; sum = (sum + pos) | 0; pos = big.indexOf(needle, pos + 1); }
  pos = big.lastIndexOf(needle);
  while (pos > 0) { found++; sum = (sum ^ pos) | 0; pos = big.lastIndexOf(needle, pos - 1); }
  sum = (sum + (big.includes(needle, big.length >> 1) ? 7 : 3)) | 0;
}
for (let i = 0; i < 4000; i++) {
  const needle = PIECES[i % PIECES.length];
  const from = (i * 7919) % big.length;
  sum = (sum + big.indexOf(needle, from) - big.lastIndexOf(needle, from)) | 0;
  sum = (sum + big.codePointAt(from) + big.charCodeAt((from * 3) % big.length)) | 0;
}
log("search", found, sum, big.length);
// Positions inside a surrogate pair.
const pair = "a😀b😀c";
for (let i = 0; i <= pair.length; i++) {
  log("pair", i, pair.indexOf("😀", i), pair.lastIndexOf("😀", i), pair.indexOf("\ude00", i), pair.lastIndexOf("\ud83d", i), pair.includes("b", i));
}

// One-code-point strings from iteration, charAt, at, spread and regex.
const text = "naïve café 日本語 😀🎉 Ελλάδα Москва ß \ud800 x";
const seen = new Map();
for (let r = 0; r < 50; r++) {
  for (const ch of text) seen.set(ch, (seen.get(ch) || 0) + 1);
  for (let i = 0; i < text.length; i++) { const c = text.charAt(i); const a = text.at(i); if (c !== a || c !== text[i]) log("mismatch", i); }
}
log("chars", [...seen.keys()].map((k) => k.length + ":" + k.codePointAt(0).toString(16)).join(","), [...seen.values()].join(","));
let acc = "";
for (const ch of text) acc += ch + ch;
log("acc", acc.length, hash(acc), show([...text].slice(0, 6)), show(Array.from("é😀\ud800")), show(text.match(/./gu).slice(8, 14)));
const keep = [];
for (let r = 0; r < 2000; r++) { for (const ch of "éß日😀") keep.push(ch); if (keep.length > 4000) keep.length = 0; }
log("keep", keep.length, keep.slice(-4).join(""), keep[0] === "é", "é" === keep[0]);
console.log(out.join("\n"));
"##;

fn node_output(src: &str) -> Vec<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("node")
        .args(["--input-type=commonjs", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node on PATH");
    child.stdin.take().expect("stdin").write_all(src.as_bytes()).expect("write source");
    let out = child.wait_with_output().expect("node output");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn string_unicode_paths_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    let want = node_output(SOURCE);
    for (i, (a, b)) in lines.iter().zip(want.iter()).enumerate() {
        assert_eq!(a, b, "line {i} differs");
    }
    assert_eq!(lines.len(), want.len());
}

/// The same program in fresh children: no JIT, eager JIT, and GC stress (the
/// code-point cache must see every slot it names freed and reused).
#[test]
fn string_unicode_paths_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
        ("gc-jit", &[("ZIPP_GC_STRESS", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["string_unicode_paths_match_node", "--exact"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
