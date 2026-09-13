#![cfg(feature = "python")]
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};

fn run(source: &str) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut program = compile_source(
                &source,
                Frontend::Python {
                    mode: PythonMode::Module,
                },
            )?;
            let state = program.state_mut();
            state.set_limits(5_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .unwrap()
        .join()
        .unwrap()
}

#[cfg(not(feature = "python-js-interop"))]
#[test]
fn shared_scope_javascript_is_absent_by_default() {
    assert!(run("import javascript\n").is_err());
    assert!(run("from js import eval\n").is_err());
}

#[cfg(feature = "python-js-interop")]
#[test]
fn javascript_data_and_state_live_in_the_same_vm() {
    let output = run(r#"
import javascript
from js import eval as js_eval
print(javascript.engine)
print(javascript.eval('[1, 2, 3].map(x => x * 2)'))
print(javascript.eval('globalThis.sharedCounter = 41'))
print(js_eval('++globalThis.sharedCounter'))
print(javascript.eval('({name:"Zipp", nested:[true,null,3.5], ["__proto__"]:7})'))
print(js_eval('typeof document'), js_eval('typeof process'))
"#)
    .unwrap();
    assert_eq!(output[0..4], ["zipp-same-vm", "[2, 4, 6]", "41", "42"]);
    assert!(output[4].contains("'__proto__': 7"));
    assert_eq!(output[5], "undefined undefined");
    assert_eq!(
        run("import javascript\nprint(javascript.eval('typeof sharedCounter'))\n").unwrap(),
        ["undefined"]
    );
}

#[cfg(feature = "python-js-interop")]
#[test]
fn javascript_results_and_errors_are_bounded() {
    let output = run(r#"
import javascript
for source in ['(()=>1)', '(()=>{const a={};a.a=a;return a})()', '({get x(){return 1}})', 'Infinity']:
    try:
        javascript.eval(source)
    except TypeError:
        print('data rejected')
try:
    javascript.eval('throw new Error("expected")')
except RuntimeError:
    print('JS error')
print(javascript.eval('6 * 7'))
"#).unwrap();
    assert_eq!(
        output,
        [
            "data rejected",
            "data rejected",
            "data rejected",
            "data rejected",
            "JS error",
            "42"
        ]
    );
    assert!(run("import javascript\njavascript.eval('while (true) {}')\n").is_err());
}
