//! Ordinary returns skip the out-of-line finally-router only when the current
//! frame's handler stack is empty.

const RETURN_SEMANTICS: &str = r#"
    var log = [];

    function plain(v) { return v; }
    function implicit(flag) { if (flag) return; }

    function throughFinally(v) {
      try { return v; }
      finally { log.push("finally:" + v); }
    }

    function undefinedThroughFinally() {
      try { return; }
      finally { log.push("finally:undefined"); }
    }

    function catchOnly(v) {
      try { return v; }
      catch (e) { log.push("wrong-catch"); return -1; }
    }

    function returnInsideCatch(v) {
      try {
        try { throw v; }
        catch (e) { return "caught:" + e; }
      } finally {
        log.push("outer:" + v);
      }
    }

    function nested(v) {
      try {
        return (function inner() { return v + 1; })();
      } finally {
        log.push("nested:" + v);
      }
    }

    console.log(
      plain(3),
      implicit(true),
      implicit(false),
      throughFinally(4),
      undefinedThroughFinally(),
      catchOnly(5),
      returnInsideCatch(6),
      nested(7)
    );
    console.log(log.join(","));
"#;

#[test]
fn empty_handler_fast_path_preserves_return_semantics() {
    let out = zipp_vm::run(RETURN_SEMANTICS).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(
        out.output,
        [
            "3 undefined undefined 4 undefined 5 caught:6 8",
            "finally:4,finally:undefined,outer:6,nested:7",
        ]
    );
}
