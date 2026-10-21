//! `import.meta.url` is each module file's `file:` URL (#76), as Node, Deno,
//! Bun and browsers give it: the entry module's, an imported module's (the
//! same object again through a dynamic `import()`), percent-encoded as Node's
//! `url.pathToFileURL` writes it. Code that was not loaded from a file has
//! none.

#![cfg(not(feature = "wasm-no-fs-loader"))]

use std::path::PathBuf;

fn module_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zipp meta url #1 %é {}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the module directory");
    dir
}

fn run(main: &str, dep: &str) -> Vec<String> {
    let dir = module_dir();
    std::fs::write(dir.join("main.mjs"), main).expect("write main.mjs");
    std::fs::write(dir.join("dep.mjs"), dep).expect("write dep.mjs");
    let path = dir.join("main.mjs");
    let out = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || zipp_vm::run_module_file(&path, None))
        .expect("spawn")
        .join()
        .expect("join")
        .expect("the module runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.error.is_none(), "error: {:?}", out.error);
    out.output
}

#[test]
fn every_module_file_gets_its_url() {
    let out = run(
        r#"
        import { depUrl, depLater } from "./dep.mjs";
        console.log(import.meta.url);
        console.log(depUrl);
        console.log(depLater());
        console.log(typeof import.meta.url, Object.getPrototypeOf(import.meta));
        const d = Object.getOwnPropertyDescriptor(import.meta, "url");
        console.log(d.writable, d.enumerable, d.configurable);
        const ns = await import("./dep.mjs");
        console.log(ns.depUrl === depUrl);
        "#,
        "export const depUrl = import.meta.url;\nexport function depLater() { return import.meta.url; }\n",
    );
    let segment = format!("zipp%20meta%20url%20%231%20%25%C3%A9%20{}", std::process::id());
    assert!(out[0].starts_with("file://"), "{}", out[0]);
    assert!(out[0].ends_with(&format!("/{segment}/main.mjs")), "{}", out[0]);
    assert!(out[1].ends_with(&format!("/{segment}/dep.mjs")), "{}", out[1]);
    assert_eq!(out[0].strip_suffix("main.mjs"), out[1].strip_suffix("dep.mjs"));
    assert_eq!(out[2], out[1]);
    assert_eq!(&out[3..], ["string null", "true true true", "true"]);
}

#[test]
fn source_text_modules_have_no_url() {
    let out = zipp_vm::run_module_with_base("console.log(typeof import.meta.url);", None).expect("runs");
    assert!(out.error.is_none(), "error: {:?}", out.error);
    assert_eq!(out.output, ["undefined"]);
}
