#![cfg(feature = "safe-sandbox")]

use std::process::Command;

const PROBE_ENV: &str = "ZIPP_HOSTILE_SYNTAX_PROBE";

fn hostile_source(shape: &str, depth: usize) -> String {
    match shape {
        "parentheses" => format!("{}0{};", "(".repeat(depth), ")".repeat(depth)),
        "unary" => format!("{}0;", "!".repeat(depth)),
        "assignment" => format!("{}0;", "a=".repeat(depth)),
        "conditional" => format!("{}0;", "0?0:".repeat(depth)),
        "exponent" => format!("{}1;", "1**".repeat(depth)),
        "blocks" => format!("{}0;{}", "{".repeat(depth), "}".repeat(depth)),
        "functions" => format!("{}0;{}", "function f(){".repeat(depth), "}".repeat(depth)),
        "members" => format!("a{};", ".x".repeat(depth)),
        "binary" => format!("{}1;", "1+".repeat(depth)),
        "composite" => {
            // Each individual grammar limit remains below its cap, but their
            // independent function/body/member/operator edges compose into a
            // tree deeper than any recursive compiler walk is allowed to see.
            let functions = depth.min(10);
            let mut source = "function f(){".repeat(functions);
            source.push('a');
            source.push_str(&".x".repeat(16));
            source.push_str(&"+1".repeat(16));
            source.push(';');
            source.push_str(&"}".repeat(functions));
            source
        }
        "pattern" => format!("let {}x{} = [];", "[".repeat(depth), "]".repeat(depth)),
        "regex-groups" => format!("/{}a{}/;", "(?:".repeat(depth), ")".repeat(depth)),
        "regex-alternatives" => format!("/{}a/;", "a|".repeat(depth)),
        other => panic!("unknown hostile syntax probe {other}"),
    }
}

/// The parent tests below re-exec this integration-test binary for every crash
/// shape. A regression that restores a native stack overflow aborts only the
/// child, and is reported as an ordinary failed test rather than taking the
/// whole test process with it.
#[test]
fn hostile_syntax_child() {
    let Ok(shape) = std::env::var(PROBE_ENV) else {
        return;
    };
    let source = hostile_source(&shape, 2_000);
    let result = std::thread::Builder::new()
        .name(format!("syntax-probe-{shape}"))
        // Match the 1 MiB stack used by the hardened Wasm build and the
        // smallest native host thread we support.
        .stack_size(1024 * 1024)
        .spawn(move || zipp_vm::compile_to_text(&source, false))
        .expect("spawn bounded-stack parser probe")
        .join()
        .expect("parser/compiler must return instead of overflowing its stack");
    let error = result.expect_err("hostile nesting must fail closed");
    assert!(
        error.contains("sandbox limit"),
        "unexpected error for {shape}: {error}"
    );
    if shape == "composite" {
        assert!(
            error.contains("compiled syntax nesting"),
            "composite precedence spines must reach the completed-AST validator: {error}"
        );
    }
}

fn assert_probe_is_contained(shape: &str) {
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "hostile_syntax_child", "--nocapture"])
        .env(PROBE_ENV, shape)
        .output()
        .expect("run hostile syntax child");
    assert!(
        output.status.success(),
        "{shape} probe escaped the fail-closed parser/compiler path\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn recursive_parser_and_compiler_shapes_fail_closed() {
    for shape in [
        "parentheses",
        "unary",
        "assignment",
        "conditional",
        "exponent",
        "blocks",
        "functions",
        "pattern",
    ] {
        assert_probe_is_contained(shape);
    }
}

#[test]
fn iterative_parser_chains_cannot_create_unbounded_recursive_asts() {
    for shape in ["members", "binary", "composite"] {
        assert_probe_is_contained(shape);
    }
}

#[test]
fn regex_ir_nesting_and_alternation_fail_closed() {
    for shape in ["regex-groups", "regex-alternatives"] {
        assert_probe_is_contained(shape);
    }
}

#[test]
fn dynamic_regexp_constructor_uses_the_same_nesting_limit() {
    let pattern = format!("{}a{}", "(?:".repeat(2_000), ")".repeat(2_000));
    let source = format!("new RegExp({pattern:?});");
    let outcome = zipp_vm::run(&source).expect("constructor source compiles");
    let error = outcome
        .error
        .expect("dynamic hostile pattern must throw a normal SyntaxError");
    assert!(
        error.contains("nesting exceeds the sandbox limit"),
        "{error}"
    );
}

#[test]
fn ordinary_nested_sources_remain_accepted() {
    let samples = [
        hostile_source("parentheses", 12),
        hostile_source("unary", 12),
        hostile_source("blocks", 12),
        hostile_source("members", 12),
        hostile_source("binary", 12),
        hostile_source("regex-groups", 32),
        hostile_source("regex-alternatives", 24),
        r"/\p{RGI_Emoji}/v;".to_string(),
    ];
    for source in samples {
        zipp_vm::compile_to_text(&source, false)
            .unwrap_or_else(|error| panic!("ordinary nesting was rejected: {error}"));
    }
}
