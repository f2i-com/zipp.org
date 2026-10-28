//! A TypeScript file runs as a script unless it does not parse as one, as
//! tsc and Node decide: a namespace's exported members (`export const` on an
//! indented line, which the text scan takes for a module's) and `export` in a
//! string or a comment leave it a script, while a top-level `export` makes it
//! a module. Read through `this` at the top level, which a module leaves
//! undefined; the expected lines are Node 24's
//! (`--experimental-transform-types`).

use std::path::Path;

const FILES: &[(&str, &str, &str)] = &[
    (
        "ns.ts",
        "namespace N {\n  export const k = 2;\n  export function f(): number { return k * 3; }\n}\nconsole.log(N.k, N.f(), typeof this, this === undefined);\n",
        "2 6 object false",
    ),
    (
        "mod.ts",
        "export const k: number = 1;\nconsole.log(k, typeof this, this === undefined);\n",
        "1 undefined true",
    ),
    (
        "str.ts",
        "const s: string = \"export const x\";\n// export function y() {}\nconsole.log(s.length, typeof this, this === undefined);\n",
        "14 object false",
    ),
];

fn run(path: &Path) -> Vec<String> {
    zipp_vm::set_node_runtime(true, vec!["zipp".into(), path.to_string_lossy().into_owned()]);
    let out = zipp_vm::frontend::run_typescript_file(path).expect("runs");
    assert_eq!(out.error, None, "{:?}", out.errput);
    out.output
        .iter()
        .flat_map(|l| l.lines().map(str::to_string))
        .collect()
}

#[test]
fn ts_files_are_scripts_unless_they_must_be_modules() {
    let root = std::env::temp_dir().join(format!("zipp-ts-goal-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for (name, text, want) in FILES {
        let p = root.join(name);
        std::fs::write(&p, text).unwrap();
        assert_eq!(run(&p), vec![want.to_string()], "{name}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
