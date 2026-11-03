//! A global `match` scanned in one engine pass, and the first-unit filter
//! that lets a search over UTF-16 input skip positions no match can start
//! at. Every pattern runs over ASCII, Latin-1, BMP, astral and lone-surrogate
//! subjects; the patterns cover each way a first element is derived (a
//! literal, a class, an inverted class, an alternation with an empty arm,
//! optional and repeated prefixes, lookahead and lookbehind starts, anchors,
//! word boundaries, case folding, backreferences) and the protocol around the
//! scan (lastIndex before and after, a frozen or patched regex, a subclass,
//! the legacy statics). The expected lines are Node 24's.

const SOURCE: &str = r##"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const show = (v) => JSON.stringify(v);

const SUBJECTS = [
  "", "abc 123 a1b2", "café naïve über 42 €5 $19.99 Ångström",
  "Ελλάδα Москва 日本語 한국어 x² ½ ٣٤", "emoji😀 🎉👍 🚀ship 😀😀",
  "lone \uD83D x \uDE00 y 😀", "camelCaseWord HTTPServer ſtar İstanbul",
  "a\nb\r\nc d", "aaaa", "ab".repeat(40) + "é",
];
const PATTERNS = [
  /\d+/g, /\d+/gu, /\p{L}+/gu, /\p{Lu}\p{Ll}+/gu, /./g, /./gu, /./gsu, /x*/g, /(?:)/gu,
  /(?:a|)b/g, /a?b/g, /(?:é|)\w/gu, /(?=\p{Lu})\p{L}+/gu, /(?!a)\w/g, /(?<=[$€£])\d+(?:\.\d+)?/g,
  /(?<![\d.])\d+(?![\d.])/g, /(?<!\p{L})\p{Lu}\p{Ll}+/gu, /\bcafé\b/gu, /é\b/gu, /^\w/gm, /$/g,
  /(a)\1/g, /[\u{1F300}-\u{1F6FF}]/gu, /\uD83D/g, /\uD83D/gu, /[^a ]/g, /[^a ]/gu, /É/gi, /é/giu,
  /[éÉ]/g, /ſ/giu, /s/giu, /k/giu, /\p{Script=Han}+/gu, /[\p{Script=Greek}\p{Script=Cyrillic}]+/gu,
  /(?<n>\d)(?<m>\w)?/gu, /[^]/g, /\s*[,;|]\s*|\s+/gu, /😀+/gu, /😀/g, /\u{10FFFF}|z/gu,
];
for (const s of SUBJECTS) {
  const row = [];
  for (const re of PATTERNS) {
    re.lastIndex = 3;
    const m = s.match(re);
    row.push(show(m) + "@" + re.lastIndex);
  }
  log(show(s), row.join(" "));
}

// Non-global forms and the other searches the filter serves.
for (const s of SUBJECTS) {
  log("search", show(s), s.search(/\d/), s.search(/(?<=é)./u), s.search(/\p{Lu}/u), s.search(/[😀]/u));
  log("replace", show(s.replace(/\d+/g, "#")), show(s.replace(/(?<=\p{Ll})(?=\p{Lu})/gu, "_")));
  log("split", show(s.split(/(?<=\p{Ll})(?=\p{Lu})/u)), show(s.split(/\s*[,;|]\s*|\s+/u)));
  log("exec", show(/(\p{L})(\d)?/u.exec(s)), show(/é(.)/.exec(s)));
  log("all", show([...s.matchAll(/\p{Lu}/gu)].map((m) => m.index)));
}

// Sticky global: each match must begin where the last ended.
log("gy", show("aaba".match(/a/gy)), show("éé.é".match(/é/gy)), show("😀😀x😀".match(/😀/guy)));

// lastIndex: reset to 0 by a global match, whatever it held.
const re1 = /é/g;
re1.lastIndex = { valueOf() { log("valueOf ran"); return 2; } };
log("lastIndex", show("éaé".match(re1)), re1.lastIndex);

// A frozen global regex cannot reset lastIndex: TypeError.
try { "éé".match(Object.freeze(/é/g)); log("frozen", "no throw"); } catch (e) { log("frozen", e.constructor.name); }
const ro = /é/g;
Object.defineProperty(ro, "lastIndex", { writable: false, value: 0 });
try { "éé".match(ro); log("readonly", "no throw"); } catch (e) { log("readonly", e.constructor.name); }

// A patched exec, flags getter and a subclass are honoured.
class R extends RegExp { exec(s) { log("sub exec", this.lastIndex); return super.exec(s); } }
log("subclass", show("aéaé".match(new R("é", "g"))));
const patched = /é/g;
patched.exec = function (s) { log("own exec", this.lastIndex); return RegExp.prototype.exec.call(this, s); };
log("own", show("éxé".match(patched)));
const desc = Object.getOwnPropertyDescriptor(RegExp.prototype, "global");
Object.defineProperty(RegExp.prototype, "global", { get() { log("global read"); return desc.get.call(this); }, configurable: true });
log("flags", show("éxé".match(/é/g)));
Object.defineProperty(RegExp.prototype, "global", desc);
const exec0 = RegExp.prototype.exec;
RegExp.prototype.exec = function (s) { log("proto exec"); return exec0.call(this, s); };
log("proto", show("éé".match(/é/g)));
RegExp.prototype.exec = exec0;

// Legacy statics name the last match.
"x1 é2 y3😀".match(/(\D)(\d)/gu);
log("statics", show(RegExp.lastMatch), show(RegExp.$1), show(RegExp.$2), show(RegExp.leftContext), show(RegExp.rightContext), show(RegExp.input));
"abc a1 b2".match(/([a-z])(\d)/g);
log("statics", show(RegExp.lastMatch), show(RegExp["$&"]), show(RegExp.$1), show(RegExp.leftContext), show(RegExp.rightContext));
"no match".match(/\d/g);
log("statics kept", show(RegExp.lastMatch));

// split: captures, limits, empty matches at either end, lookaround, astral.
const SPLITS = [/,/, /(,)/, /(?:)/, /(?:)/u, /x*/, /(a)|(b)/, /\s*/u, /(?<=é)/u, /\b/, /$/, /^/m,
  /(?=\p{Lu})/u, /😀/u, /(?:)/y, /,/g, /\uDE00/, /\uDE00/u, /[^]/u, /(é)?,/];
for (const s of ["", "a,b,,c,", ",", "aébc,😀,é", "😀😀", "camelCase HTTPServer", "lone\uD83Dx\uDE00,", "x"]) {
  log("splits", show(s), SPLITS.map((re) => show(s.split(re))).join(" "));
  log("limits", show(s.split(/(,)/, 2)), show(s.split(/(?:)/u, 3)), show(s.split(/,/, 0)),
    show(s.split(/(,)|(b)/, 4)), show(s.split(/,/, -1)), show(s.split(/,/, 1.5)), show(s.split(/(é)|,/u, 2)));
  s.split(/([a-z])(?=,)/);
  log("split statics", show(RegExp.lastMatch), show(RegExp.$1), show(RegExp.leftContext));
}
"q,w,e".split(/(,)/, 2);
log("limit statics", show(RegExp.lastMatch), show(RegExp.rightContext));
log("limit valueOf", show("a,b,c".split(/,/, { valueOf() { log("limit read"); return 2; } })));
class Sp extends RegExp {}
log("split subclass", show("a,b".split(new Sp(","))));
const rc = /,/;
rc.constructor = function C(src, fl) { log("ctor", fl); return new RegExp(src, fl); };
rc.constructor[Symbol.species] = rc.constructor;
log("split ctor", show("a,b".split(rc)));
const species = Object.getOwnPropertyDescriptor(RegExp, Symbol.species);
Object.defineProperty(RegExp, Symbol.species, { get() { log("species read"); return RegExp; }, configurable: true });
log("split species", show("a,b".split(/,/)));
Object.defineProperty(RegExp, Symbol.species, species);

// lastIndex read and written by hot code (compiled code takes both).
function lex(re, s) {
  let n = 0, pos = 0;
  while (pos < s.length) { re.lastIndex = pos; if (re.test(s)) { n++; pos = re.lastIndex; } else pos++; }
  return n;
}
let lexed = 0;
for (let r = 0; r < 300; r++) lexed += lex(/\w+/y, "ab cd, ef g") + lex(/é+/uy, "éé x é");
log("lex", lexed);
function setLi(re, v) { re.lastIndex = v; return re.lastIndex; }
for (let r = 0; r < 300; r++) setLi(/a/y, r);
try { setLi(Object.freeze(/a/y), 3); log("frozen set", "no throw"); } catch (e) { log("frozen set", e.constructor.name); }
const fixedLi = Object.defineProperty(/a/g, "lastIndex", { writable: false, value: 2 });
try { setLi(fixedLi, 3); log("readonly set", "no throw"); } catch (e) { log("readonly set", e.constructor.name, fixedLi.lastIndex); }
const sloppySet = new Function("re", "v", "re.lastIndex = v; return re.lastIndex;");
for (let r = 0; r < 300; r++) sloppySet(/a/g, r);
log("sloppy", sloppySet(Object.freeze(/a/g), 9), sloppySet(/a/g, "7"), typeof sloppySet(/a/g, {}));
const withLi = /a/g;
withLi.lastIndex = { valueOf() { return 1; } };
log("object li", typeof setLi(withLi, withLi.lastIndex), show("aaa".replace(withLi, "b")), withLi.lastIndex);

// A String wrapper and a non-string subject still convert.
log("wrapper", show(new String("é1é2").match(/\d/g)), show([1, 22, 333].join().match(/\d+/g)));
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
fn regexp_match_scan_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    let want = node_output(SOURCE);
    for (i, (a, b)) in lines.iter().zip(want.iter()).enumerate() {
        assert_eq!(a, b, "line {i} differs");
    }
    assert_eq!(lines.len(), want.len());
}

/// The same program in fresh children: the per-match loop (scan off), the
/// interpreter alone, the JIT compiling eagerly, and under GC stress.
#[test]
fn regexp_match_scan_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("no-scan", &[("ZIPP_NO_REGEXP_MATCH_FAST", "1")][..]),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("no-rx-jit", &[("ZIPP_NO_RX_JIT", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["regexp_match_scan_matches_node", "--exact"]);
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
