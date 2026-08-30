#![cfg(feature = "safe-sandbox")]

use std::process::Command;
use zipp_vm::safe_syntax_limits::{MAX_SAFE_SYNTAX_CHAIN, MAX_SAFE_SYNTAX_RECURSION};

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
        // The two shapes measured to be the most expensive per unit of guard
        // budget: an `else if` ladder spends one recursion level per arm but
        // ~1.3 KB of Wasm stack, and an arrow chain runs the recursion counter
        // three times ahead of the AST depth the validator sees.
        "else-if" => {
            let mut source = String::from("if(x===0){f(0);}");
            for arm in 1..depth {
                source.push_str(&format!("else if(x==={arm}){{f({arm});}}"));
            }
            source
        }
        "arrows" => format!("{}1;", "()=>".repeat(depth)),
        "composite" => {
            // Each individual grammar limit remains below its cap, but their
            // independent function/body/member/operator edges compose into a
            // tree deeper than any recursive compiler walk is allowed to see.
            //
            // Derived from the constants rather than written as literals. The
            // hard-coded 10/16/16 this replaced stopped composing past the AST
            // validator the moment the limits moved, which is the failure mode
            // this whole file exists to catch.
            let functions = depth.min(MAX_SAFE_SYNTAX_RECURSION / 8);
            let links = MAX_SAFE_SYNTAX_CHAIN - 1;
            let mut source = "function f(){".repeat(functions);
            source.push('a');
            source.push_str(&".x".repeat(links));
            source.push_str(&"+1".repeat(links));
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
        // Sized for THIS build's frames, not for the shipped artifact's. The
        // limits are calibrated against the Wasm profile's 1 MiB linker stack,
        // where the worst shape costs ~1.3 KB per recursion level; an
        // unoptimized native test binary spends roughly an order of magnitude
        // more per level, so a 1 MiB thread here is a far harsher budget than
        // anything that ships and would fail this test for a reason no user can
        // hit. MEASURED 2026-08-30 on a debug build: the worst shape below needs
        // between 2 and 4 MiB, so this is a little over 2x the requirement.
        // The native hardened runner gives its interpreter thread 256 MiB.
        .stack_size(8 * 1024 * 1024)
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
        "else-if",
        "arrows",
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
