//! The regex differential fuzzer's regression corpus
//! (`crates/regress-fork/fuzz/`): every case on which ZIPP and Node 24 were
//! found to disagree, minimized, run through the fuzzer's own harness. The
//! test pins ZIPP's output as recorded, so a change to the regex engine that
//! alters any of these answers (fixing one included) fails here until the
//! corpus line is re-recorded and the change explained.

use zipp_vm::embed;

const CORPUS: &str = include_str!("../../regress-fork/fuzz/corpus.js");
const HARNESS: &str = include_str!("../../regress-fork/fuzz/harness.js");

const CHECK: &str = r#"
var bad = 0, agree = 0;
for (var i = 0; i < CASES.length; i++) {
  var got = runOne(i);
  if (got !== ZIPP[i]) {
    bad++;
    console.log("CHANGED " + i + " " + JSON.stringify(CASES[i]));
    console.log("  recorded:" + ZIPP[i]);
    console.log("  now:     " + got);
  }
  var bare = got.indexOf(" || ") < 0 ? got : got.slice(0, got.indexOf(" || "));
  if (bare === NODE[i]) agree++;
}
console.log("SUMMARY " + CASES.length + " cases, " + bad + " changed, " + agree + " agree with Node");
"#;

#[test]
fn regex_fuzz_corpus_answers_as_recorded() {
    let source = format!("var NO_LOOP = true;\n{CORPUS}\n{HARNESS}\n{CHECK}");
    let mut state = embed::compile_script(&source).expect("corpus compiles");
    state.run_init().expect("corpus runs");
    let output = state.take_output();
    let summary = output
        .iter()
        .find(|line| line.starts_with("SUMMARY "))
        .unwrap_or_else(|| panic!("no summary line:\n{}", output.join("\n")));
    assert!(
        summary.contains(" 0 changed"),
        "{}\n{summary}",
        output.join("\n")
    );
}
