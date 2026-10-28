//! `===` and `!==` between string literals, whose shared strings are atoms
//! (`Heap::atoms`: two different atoms are never equal), and strings built at
//! run time with the same or other text: concatenation, slices, indexing, the
//! empty string, the two-digit table, `typeof`, JSON, object keys, templates,
//! `String.fromCharCode`, non-ASCII text. In compiled loops, if-chains and
//! `switch`, and through Map keys, `includes` and `indexOf`. Run
//! interpreted, at the default thresholds, with every function compiled at
//! once, under GC stress, with no literal marked and with the switch bits test
//! off; the expected lines are Node 24's.

const EXPECTED: &str = r####"acc 6545200
found 256
digits 07,42,99"####;

#[test]
fn string_atoms_child() {
    if std::env::var_os("ZIPP_ATOMS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn atoms_compare_like_node() {
    if std::env::var_os("ZIPP_ATOMS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("atoms-off", Some(("ZIPP_NO_ATOMS", "1"))),
        ("switch-bits-off", Some(("ZIPP_NO_STR_CASE_BITS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "string_atoms_child", "--nocapture"])
            .env("ZIPP_ATOMS_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// `===` and `!==` between string literals (each text's one shared string)
// and strings built at run time with the same or other text: concatenation,
// slices, single characters from indexing, the empty string, two-digit
// strings (an engine table of its own), `typeof`, JSON, object keys, number
// formatting, template literals, `String.fromCharCode`. In a loop long
// enough to be compiled, and through Map keys, `includes`, `indexOf` and
// `switch`.
const out = [];
const lits = ["num", "var", "bin", "call", "", "a", "z", "07", "42", "number", "string", "undefined", "key", "é", "€uro", "x".repeat(0)];
function builtFor(s, i) {
  switch (i % 9) {
    case 0: return s.split("").join("");
    case 1: return (s + "#").slice(0, s.length);
    case 2: return JSON.parse(JSON.stringify(s));
    case 3: return `${s}`;
    case 4: return [...s].join("");
    case 5: return Object.keys({ [s]: 1 })[0];
    case 6: return s.length ? String.fromCharCode(...[...s].map((c) => c.charCodeAt(0))) : "";
    case 7: return s.length === 2 && /^\d\d$/.test(s) ? String(Number(s)).padStart(2, "0") : s.concat("");
    default: return s.substring(0);
  }
}
function eqs(a, b) { return (a === b ? 1 : 0) + (a !== b ? 2 : 0); }
function classify(k) {
  if (k === "num") return 1;
  if (k === "var") return 2;
  if (k === "") return 3;
  if (k === "a") return 4;
  if (k === "07") return 5;
  if (k === "number") return 6;
  if (k === "é") return 7;
  return 0;
}
let acc = 0;
const tally = [];
for (let r = 0; r < 400; r++) {
  for (let i = 0; i < lits.length; i++) {
    const a = lits[i];
    const b = builtFor(a, i + r);
    for (let j = 0; j < lits.length; j++) {
      acc += eqs(a, lits[j]) * (j + 1) + eqs(b, lits[j]) * (j + 3) + eqs(lits[j], b) * (j + 5);
    }
    acc += classify(a) * 7 + classify(b) * 11;
    acc += eqs(typeof r, "number") + eqs(typeof a, "string") + eqs(typeof undefined, "undefined");
    acc += eqs(a, b) * 13;
  }
}
out.push("acc " + acc);
const m = new Map(lits.map((s, i) => [s, i]));
let found = 0;
for (let i = 0; i < lits.length; i++) {
  const b = builtFor(lits[i], i);
  found += (m.get(b) ?? -100) + (lits.includes(b) ? 1 : 0) + lits.indexOf(b);
}
out.push("found " + found);
const digits = [];
for (let i = 0; i < 100; i++) digits.push(String(i).padStart(2, "0"));
out.push("digits " + digits.filter((d) => d === "07" || d === "42" || d === "99").join(","));
console.log(out.join("\n"));
"####;
