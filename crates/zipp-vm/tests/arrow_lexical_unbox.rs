//! Exactness coverage for removing unnecessary heap cells from block-bodied
//! arrow locals. Captured/direct-eval/forward-reference bindings must retain
//! their cells; only a simple declaration reached before any reference may use
//! the ordinary register path.

use std::process::Command;

use zipp_vm::{compile_to_text, run};

const COPY_SOURCE: &str = r#"
    "use strict";
    const copy = (input = "abc") => {
        let out = "";
        let i = 0;
        while (i < input.length) {
            out += input[i];
            i++;
        }
        return out;
    };
    console.log(copy("zipp"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let outcome = run(src).expect("source compiles");
    assert!(
        outcome.error.is_none(),
        "runtime error: {:?}",
        outcome.error
    );
    outcome.output
}

#[test]
fn semantics_keep_captures_eval_and_forward_tdz_boxed() {
    let src = r#"
        "use strict";
        const captured = () => {
            let x = 1;
            const read = () => x;
            x = 7;
            return read();
        };
        const forward = () => {
            let before;
            try { before = String(x); } catch (e) { before = e.name; }
            let x = 9;
            return before + "/" + x;
        };
        const assigned = () => {
            let before;
            try { x = 4; } catch (e) { before = e.name; }
            let x = 11;
            return before + "/" + x;
        };
        const direct = () => {
            let x = 2;
            eval("x = 13");
            return x;
        };
        console.log(captured(), forward(), assigned(), direct());
    "#;

    assert_eq!(run_ok(src), ["7 ReferenceError/9 ReferenceError/11 13"]);
    let bc = compile_to_text(src, false).expect("source compiles");
    assert!(
        bc.contains("MakeCellTdz"),
        "required cell lowering absent:\n{bc}"
    );
    assert!(
        bc.contains("CellSetChecked"),
        "forward TDZ write check absent:\n{bc}"
    );
}

#[test]
fn bytecode_shape_child() {
    let Some(mode) = std::env::var_os("ZIPP_ARROW_UNBOX_BC_CHILD") else {
        return;
    };
    let bc = compile_to_text(COPY_SOURCE, false).expect("source compiles");
    if mode == "on" {
        assert!(
            !bc.contains("MakeCellTdz"),
            "uncaptured locals stayed boxed:\n{bc}"
        );
        assert!(
            bc.contains("StrAppendIndex"),
            "local string fusion absent:\n{bc}"
        );
    } else {
        assert!(
            bc.contains("MakeCellTdz"),
            "historical cell lowering absent:\n{bc}"
        );
        assert!(
            !bc.contains("StrAppendIndex"),
            "off switch still fused cell append:\n{bc}"
        );
    }
    assert_eq!(run_ok(COPY_SOURCE), ["zipp"]);
}

#[test]
fn bytecode_on_and_off_switch_are_exact() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, off) in [("on", false), ("off", true)] {
        let mut cmd = Command::new(&exe);
        cmd.args(["bytecode_shape_child", "--exact"])
            .env("ZIPP_ARROW_UNBOX_BC_CHILD", mode)
            .env_remove("ZIPP_NO_ARROW_LEXICAL_UNBOX");
        if off {
            cmd.env("ZIPP_NO_ARROW_LEXICAL_UNBOX", "1");
        }
        let out = cmd.output().expect("spawn bytecode child");
        assert!(
            out.status.success(),
            "{mode} bytecode child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
