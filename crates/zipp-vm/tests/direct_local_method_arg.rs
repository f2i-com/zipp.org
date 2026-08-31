//! A fused one-argument named method call can use a stable local register as
//! its argument window instead of dispatching `Move local -> arg_temp` first.
//! Parameters, cells, eval-visible bindings, and receiver/result overlaps keep
//! the historical staged window.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn text_of(src: &str) -> String {
    zipp_vm::compile_to_text(src, false).expect("source compiles")
}

fn move_count(src: &str) -> usize {
    text_of(src).matches("Move {").count()
}

#[test]
fn direct_local_argument_preserves_method_semantics_and_active_arguments() {
    let out = run_ok(
        r#"
        function direct() {
          var text = "abcd";
          var index = 2;
          return text.charCodeAt(index);
        }
        function append() {
          var values = [];
          var value = 7;
          values.push(value);
          return values.length + ":" + values[0];
        }
        function target(x) {
          return this.tag + ":" + target.arguments[0] + ":" + x;
        }
        function activeArguments() {
          var receiver = {tag: "T", method: target};
          var value = 9;
          return receiver.method(value);
        }
        function getterOrder() {
          var events = [];
          var value = 5;
          var receiver = {};
          Object.defineProperty(receiver, "method", {
            get: function () {
              events.push("get");
              return function (x) { events.push("call:" + x); return x; };
            }
          });
          var result = receiver.method(value);
          return events.join("|") + ":" + result;
        }
        console.log(direct(), append(), activeArguments(), getterOrder());
        "#,
    );
    assert_eq!(out, ["99 1:7 T:9:9 get|call:5:5"]);
}

#[test]
fn captured_and_eval_visible_arguments_keep_their_values() {
    let out = run_ok(
        r#"
        function parameter(text, index) { return text.charCodeAt(index); }
        function captured() {
          var text = "abcd";
          var index = 1;
          function read() { return index; }
          return text.charCodeAt(index) + read();
        }
        function evaluated() {
          var text = "abcd";
          var index = 0;
          eval("index = 3");
          return text.charCodeAt(index);
        }
        console.log(parameter("abcd", 2), captured(), evaluated());
        "#,
    );
    assert_eq!(out, ["99 99 100"]);
}

#[test]
fn bytecode_shape_uses_the_local_directly_only_when_safe() {
    let direct = r#"
        function direct() {
          var text = "abcd";
          var index = 1;
          return text.charCodeAt(index);
        }
        direct();
    "#;
    let direct_text = text_of(direct);
    assert!(
        direct_text.contains("CallMethod"),
        "probe stopped using fused CallMethod:\n{direct_text}"
    );
    assert_eq!(
        move_count(direct),
        0,
        "the stable non-parameter local should not be staged:\n{direct_text}"
    );

    // A simple parameter may alias a sloppy mapped `arguments[index]` entry;
    // keep the snapshot window even when the direct-local optimization is on.
    let parameter =
        "function parameter(text, index) { return text.charCodeAt(index); } parameter('abcd', 1);";
    assert_eq!(
        move_count(parameter),
        1,
        "a parameter argument must remain staged:\n{}",
        text_of(parameter)
    );

    // Assign-in-place can hand `call` the local itself as `dst`; keep its input
    // in a distinct window until the result write completes.
    let result_overlap = r#"
        function resultOverlap() {
          var text = "abcd";
          var index = 1;
          index = text.charCodeAt(index);
          return index;
        }
        resultOverlap();
    "#;
    assert_eq!(
        move_count(result_overlap),
        2,
        "a result-overlapping local must remain staged:\n{}",
        text_of(result_overlap)
    );

    // Captured and direct-eval-visible locals are cells, never direct windows.
    let captured = text_of(
        "function f(){var s='ab', i=1; function read(){return i;} return s.charCodeAt(i)+read();} f();",
    );
    assert!(
        captured.contains("CellGet") && captured.contains("CallMethod"),
        "captured argument stopped using a cell-backed read:\n{captured}"
    );
    let eval_visible =
        text_of("function f(){var s='ab', i=0; eval('i=1'); return s.charCodeAt(i);} f();");
    assert!(
        eval_visible.contains("CellGet") && eval_visible.contains("CallMethod"),
        "eval-visible argument stopped using a cell-backed read:\n{eval_visible}"
    );
}
