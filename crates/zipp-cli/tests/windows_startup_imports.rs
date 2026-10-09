//! The Windows loader maps and initialises every DLL in a binary's import
//! table before `main` runs. The `gpu` feature's runtime imports four system
//! DLLs that only a program using the GPU ever calls; as ordinary imports
//! they made every launch 2 ms slower (7.0 ms against 4.9 ms). `build.rs`
//! delay-loads them, and these checks read the linked executable so that a
//! linker or dependency change cannot quietly move them back.

#![cfg(all(windows, target_env = "msvc"))]

use std::fs;
use std::path::Path;
use std::process::Command;

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("u16 in PE"))
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 in PE"))
}

struct Pe {
    bytes: Vec<u8>,
    /// `(virtual address, virtual size, file offset, file size)` per section.
    sections: Vec<(u32, u32, u32, u32)>,
    directories: usize,
    directory_count: usize,
}

impl Pe {
    fn read(path: &Path) -> Self {
        let bytes = fs::read(path).expect("read zipp executable");
        assert_eq!(bytes.get(..2), Some(&b"MZ"[..]), "DOS signature");
        let pe = u32_at(&bytes, 0x3c) as usize;
        assert_eq!(bytes.get(pe..pe + 4), Some(&b"PE\0\0"[..]), "PE signature");
        let coff = pe + 4;
        let section_count = u16_at(&bytes, coff + 2) as usize;
        let optional = coff + 20;
        let optional_size = u16_at(&bytes, coff + 16) as usize;
        // The data directories follow NumberOfRvaAndSizes, whose offset
        // depends on the optional header's flavour.
        let count_at = match u16_at(&bytes, optional) {
            0x20b => optional + 108,
            0x10b => optional + 92,
            magic => panic!("unexpected PE optional-header magic {magic:#x}"),
        };
        let directory_count = u32_at(&bytes, count_at) as usize;
        let section_table = optional + optional_size;
        let sections = (0..section_count)
            .map(|i| {
                let at = section_table + 40 * i;
                (
                    u32_at(&bytes, at + 12),
                    u32_at(&bytes, at + 8),
                    u32_at(&bytes, at + 20),
                    u32_at(&bytes, at + 16),
                )
            })
            .collect();
        Pe {
            bytes,
            sections,
            directories: count_at + 4,
            directory_count,
        }
    }

    fn offset(&self, rva: u32) -> usize {
        let &(address, _, file_offset, _) = self
            .sections
            .iter()
            .find(|&&(address, size, _, file_size)| {
                rva >= address && rva - address < size.max(file_size)
            })
            .unwrap_or_else(|| panic!("RVA {rva:#x} is in no section"));
        (rva - address + file_offset) as usize
    }

    fn name(&self, rva: u32) -> String {
        let start = self.offset(rva);
        let length = self.bytes[start..]
            .iter()
            .position(|&byte| byte == 0)
            .expect("NUL-terminated DLL name");
        String::from_utf8_lossy(&self.bytes[start..start + length]).to_ascii_lowercase()
    }

    /// Lower-cased DLL names of one descriptor table: data directory `index`,
    /// `entry` bytes per descriptor, name RVA at `name_at`, ended by a
    /// descriptor whose name RVA is zero.
    fn dll_names(&self, index: usize, entry: usize, name_at: usize) -> Vec<String> {
        if index >= self.directory_count {
            return Vec::new();
        }
        let table = u32_at(&self.bytes, self.directories + 8 * index);
        if table == 0 {
            return Vec::new();
        }
        let mut at = self.offset(table);
        let mut names = Vec::new();
        loop {
            let name = u32_at(&self.bytes, at + name_at);
            if name == 0 {
                return names;
            }
            names.push(self.name(name));
            at += entry;
        }
    }

    /// DLLs the loader binds before `main`.
    fn eager_imports(&self) -> Vec<String> {
        self.dll_names(1, 20, 12)
    }

    /// DLLs bound by their first call.
    fn delayed_imports(&self) -> Vec<String> {
        self.dll_names(13, 32, 4)
    }
}

#[test]
fn zipp_binary_does_not_load_gpu_dlls_at_startup() {
    let pe = Pe::read(Path::new(env!("CARGO_BIN_EXE_zipp")));
    let eager = pe.eager_imports();
    assert!(
        eager.iter().any(|name| name == "kernel32.dll"),
        "import table was not read: {eager:?}"
    );
    for dll in build_script::WINDOWS_GPU_DELAY_LOAD_DLLS {
        assert!(
            !eager.iter().any(|name| name == dll),
            "{dll} is loaded before main: {eager:?}"
        );
    }
    // With the feature on, the runtime is linked and deferred, not absent.
    if cfg!(feature = "gpu") {
        let delayed = pe.delayed_imports();
        assert!(
            delayed.iter().any(|name| name == "dxgi.dll"),
            "dxgi.dll is not delay-loaded: {delayed:?}"
        );
    }
}

#[test]
fn zipp_binary_still_starts_and_runs_a_script() {
    let version = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .arg("--version")
        .output()
        .expect("run zipp --version");
    assert!(version.status.success());

    let path = std::env::temp_dir().join(format!(
        "zipp-windows-startup-imports-{}.js",
        std::process::id()
    ));
    fs::write(&path, "console.log(6 * 7);").expect("write probe");
    let output = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .arg("js")
        .arg(&path)
        .output()
        .expect("run zipp js");
    let _ = fs::remove_file(&path);
    assert!(
        output.status.success(),
        "zipp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}
