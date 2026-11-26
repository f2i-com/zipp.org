//! A short string read through a raw pointer by compiled code.
//!
//! A string of 23 bytes or fewer keeps its bytes in its heap slot. A loop
//! region that pins a string for `charCodeAt` reads it through a pointer to
//! its bytes, and `charCodeAt`'s own helper moves a short string's bytes to a
//! buffer, overwriting the ones in the slot with that buffer's address,
//! length and capacity. A region that had taken the slot's address then read
//! the address as characters: wrong sums, different on every run, for any
//! short string made by `slice`, `split` or concatenation (0.0.41).
//!
//! The expected lines are Node 24's.

const ALIAS: &str = r#"
function sum2(t, n) {
  let a = 0, b = 0;
  for (let i = 0; i < n; i++) {
    const u = t;
    a += t.charCodeAt(i % 7);
    b += u.charCodeAt(i % 7);
  }
  return a + ":" + b;
}
const src = "xxkey=valyy-and-more-padding-to-make-it-long";
console.log(sum2(src.slice(2, 9), 60000));
console.log(sum2("a,token_9,b".split(",")[1], 60000));
console.log(sum2("tok" + "en".toUpperCase(), 60000));
console.log(sum2(src.slice(2, 30), 60000));
"#;

const TOKENIZER: &str = r#"
function isDigit(s, i) { const c = s.charCodeAt(i); return c >= 48 && c <= 57; }
function scanField(s, rounds) {
  let letters = 0, digits = 0, hash = 0;
  for (let r = 0; r < rounds; r++) {
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      hash = (hash * 31 + c) | 0;
      if (isDigit(s, i)) digits++; else letters++;
    }
  }
  return letters + ":" + digits + ":" + hash;
}
const line = "id=42;name=alice07;role=admin";
for (const field of line.split(";")) console.log(field, scanField(field, 12000));
"#;

// The heap's object table grows under the loop: an address inside it would
// not survive that either.
const HEAP_GROWS: &str = r#"
function scan(s, n) {
  const keep = [];
  let h = 0;
  for (let i = 0; i < n; i++) {
    h = (h * 31 + s.charCodeAt(i % s.length)) | 0;
    if ((i & 7) === 0) keep.push({ i: i, t: "p" + i });
  }
  return h + ":" + keep.length;
}
const line = "alpha=1;beta=22;gamma=333";
for (const field of line.split(";")) console.log(field, scan(field, 120000));
"#;

// The append cursor keeps its source string's address across iterations.
const APPEND_FROM_SHORT_SOURCE: &str = r#"
function spell(s, rounds) {
  let out = "";
  const keep = [];
  for (let r = 0; r < rounds; r++) {
    for (let i = 0; i < s.length; i++) out += s[i];
    if ((r & 15) === 0) keep.push([r, "k" + r]);
  }
  let h = 0;
  for (let i = 0; i < out.length; i++) h = (h * 33 + out.charCodeAt(i)) | 0;
  return out.length + ":" + h + ":" + keep.length;
}
const line = "ab,cdefg,hijklmnopqrstuvw";
for (const field of line.split(",")) console.log(field, spell(field, 9000));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn short_str_pin_two_names_for_one_string() {
    assert_eq!(
        run_ok(ALIAS),
        ["6111452:6111452", "5974321:5974321", "NaN:NaN", "6111452:6111452"]
    );
}

#[test]
fn short_str_pin_direct_and_through_a_helper() {
    assert_eq!(
        run_ok(TOKENIZER),
        [
            "id=42 36000:24000:-838139904",
            "name=alice07 120000:24000:1230363744",
            "role=admin 120000:0:-598338816",
        ]
    );
}

#[test]
fn short_str_pin_while_the_heap_grows() {
    assert_eq!(
        run_ok(HEAP_GROWS),
        [
            "alpha=1 -273649697:15000",
            "beta=22 -1962122075:15000",
            "gamma=333 2055881558:15000",
        ]
    );
}

#[test]
fn short_str_append_cursor_source() {
    assert_eq!(
        run_ok(APPEND_FROM_SHORT_SOURCE),
        [
            "ab 18000:2017453432:563",
            "cdefg 45000:-1891058328:563",
            "hijklmnopqrstuvw 144000:-1893044544:563",
        ]
    );
}

/// The same answers with the JIT compiling at once and with no native code.
#[test]
fn short_str_pin_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("short_str_")
            .arg("--skip")
            .arg("all_modes")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
