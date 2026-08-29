use std::process::Command;

fn write_script(name: &str, source: &str) -> std::path::PathBuf {
    let unique = format!(
        "zipp-pgo-cli-{}-{}-{name}.js",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(&path, source).expect("write CLI fixture");
    path
}

#[test]
fn hidden_pgo_training_flag_runs_main_but_denies_runtime_compilation() {
    let binary = env!("CARGO_BIN_EXE_zipp");
    let ordinary = write_script("ordinary", "console.log('MAIN');\n");
    let ordinary_output = Command::new(binary)
        .args(["js", "--pgo-training"])
        .arg(&ordinary)
        .output()
        .expect("run PGO main source");
    std::fs::remove_file(&ordinary).expect("remove ordinary fixture");
    assert!(ordinary_output.status.success());
    assert_eq!(ordinary_output.stdout, b"MAIN\n");
    assert!(ordinary_output.stderr.is_empty());

    let dynamic = write_script(
        "dynamic",
        "globalThis['e' + 'val'](\"console.log('LEAK')\");\n",
    );
    let dynamic_output = Command::new(binary)
        .args(["js", "--pgo-training"])
        .arg(&dynamic)
        .output()
        .expect("run denied dynamic source");
    std::fs::remove_file(&dynamic).expect("remove dynamic fixture");
    assert!(!dynamic_output.status.success());
    assert!(dynamic_output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&dynamic_output.stderr).contains("LEAK"));
    assert!(String::from_utf8_lossy(&dynamic_output.stderr)
        .contains("external code is disabled by the host"));

    let help = Command::new(binary)
        .arg("--help")
        .output()
        .expect("read CLI help");
    assert!(help.status.success());
    assert!(!String::from_utf8_lossy(&help.stdout).contains("--pgo-training"));
}
