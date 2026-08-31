//! The Windows CLI runs the engine on its process main thread. These checks
//! make the corresponding PE stack contract executable: linker/profile changes
//! cannot silently restore the 1 MiB default or turn the reserve into a large
//! eager commit, and a representative recursion path must stay catchable.

#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::process::Command;

const STACK_RESERVE: u64 = 256 * 1024 * 1024;
const STACK_COMMIT: u64 = 4 * 1024;

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("u16 in PE"))
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 in PE"))
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 in PE"))
}

fn pe_stack_sizes(path: &Path) -> (u64, u64) {
    let bytes = fs::read(path).expect("read zipp executable");
    assert_eq!(bytes.get(..2), Some(&b"MZ"[..]), "DOS signature");
    let pe = u32_at(&bytes, 0x3c) as usize;
    assert_eq!(bytes.get(pe..pe + 4), Some(&b"PE\0\0"[..]), "PE signature");
    let optional = pe + 4 + 20;
    match u16_at(&bytes, optional) {
        // PE32+: SizeOfStackReserve/Commit are adjacent u64 fields at 0x48.
        0x20b => (
            u64_at(&bytes, optional + 0x48),
            u64_at(&bytes, optional + 0x50),
        ),
        // PE32: the same fields are adjacent u32 values.
        0x10b => (
            u32_at(&bytes, optional + 0x48) as u64,
            u32_at(&bytes, optional + 0x4c) as u64,
        ),
        magic => panic!("unexpected PE optional-header magic {magic:#x}"),
    }
}

#[test]
fn zipp_binary_reserves_the_guarded_engine_stack_lazily() {
    let executable = Path::new(env!("CARGO_BIN_EXE_zipp"));
    assert_eq!(pe_stack_sizes(executable), (STACK_RESERVE, STACK_COMMIT));
}

#[test]
fn interpreter_recursion_is_catchable_on_the_process_main_thread() {
    let source = r#"
        "use strict";
        function inf() { return inf2(); }
        function inf2() { return inf(); }
        try { inf(); console.log("none"); } catch (error) {
            console.log("caught:" + (error instanceof RangeError));
        }
        function down(n) { if (n <= 0) return 0; return down(n - 1) + 1; }
        console.log("deep:" + down(10000));

        // These routes recurse through Rust call machinery rather than the
        // VM's flat frame Vec. Transparent Proxy/bound forwarding matches the
        // pre-land A/B safety probe; observable apply traps additionally nest
        // native-to-JS run-loop re-entry, whose debug-sized frames are why the
        // PE reserve is materially larger than the Windows 1 MiB default.
        function F() { return 7; }
        var proxyCall = F;
        var boundCall = F;
        for (var i = 0; i < 512; i++) {
            proxyCall = new Proxy(proxyCall, {});
            boundCall = boundCall.bind(null);
        }
        console.log("proxy:" + proxyCall());
        console.log("bound:" + boundCall());

        var handler = {
            apply: function (target, thisArg, args) {
                return Reflect.apply(target, thisArg, args);
            }
        };
        var trappedCall = F;
        for (var j = 0; j < 32; j++) trappedCall = new Proxy(trappedCall, handler);
        console.log("trapped:" + trappedCall());
    "#;
    let path =
        std::env::temp_dir().join(format!("zipp-windows-main-stack-{}.js", std::process::id()));
    fs::write(&path, source).expect("write recursion probe");
    let output = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .arg("js")
        .arg(&path)
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("run zipp recursion probe");
    let _ = fs::remove_file(&path);

    assert!(
        output.status.success(),
        "zipp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "caught:true",
            "deep:10000",
            "proxy:7",
            "bound:7",
            "trapped:7"
        ]
    );
}
