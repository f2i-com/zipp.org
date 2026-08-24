//! Fail-closed runner for untrusted JavaScript.
//!
//! The public command is a small supervisor. It starts a fresh copy of this
//! executable with a minimal environment, drains a bounded amount of output,
//! and kills the child at the wall deadline. The child attaches the VM's step
//! and heap meters and disables both native JITs before parsing user code.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MAX_STEPS: u64 = 50_000_000;
const DEFAULT_MAX_HEAP_MB: usize = 128;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1 << 20;
const MAX_SOURCE_BYTES: u64 = 16 << 20;
const MAX_DYNAMIC_SOURCE_BYTES: usize = 64 << 10;
const MAX_DYNAMIC_TOTAL_SOURCE_BYTES: usize = 1 << 20;
const MAX_DYNAMIC_CALLS: usize = 256;
const MAX_DYNAMIC_FUNCTIONS: usize = 4_096;
const MAX_DYNAMIC_CLASSES: usize = 1_024;
const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1_000;
const MAX_OUTPUT_BYTES: usize = 64 << 20;

#[derive(Clone)]
struct Config {
    script: PathBuf,
    import_root: Option<PathBuf>,
    timeout_ms: u64,
    max_steps: u64,
    max_heap_mb: usize,
    max_output_bytes: usize,
}

struct ChildGuard {
    child: std::process::Child,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn kill(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
        }
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    run_supervisor(args).map_err(|error| sanitize_diagnostic(&error))
}

fn run_supervisor(args: &[String]) -> Result<(), String> {
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }
    let config = parse_public(args)?;
    validate_script(&config.script)?;

    let exe =
        std::env::current_exe().map_err(|e| format!("cannot locate the zipp executable: {e}"))?;
    // The child process must not start in the untrusted script directory. On
    // Windows the process cwd participates in DLL search and device/path
    // resolution before the VM has installed any of its language-level
    // confinement. The executable's install directory is the trusted launch
    // location; the canonical absolute script path and the explicit module
    // base below keep relative imports script-relative instead of cwd-relative.
    let child_cwd = exe
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or("cannot determine the zipp executable directory")?;

    let mut command = Command::new(&exe);
    command
        .arg("__sandbox-child")
        .arg("--max-steps")
        .arg(config.max_steps.to_string())
        .arg("--max-heap-mb")
        .arg(config.max_heap_mb.to_string())
        .arg("--max-output-bytes")
        .arg(config.max_output_bytes.to_string());
    if let Some(root) = &config.import_root {
        command.arg("--import-root").arg(root);
    }
    command
        .arg("--")
        .arg(&config.script)
        .current_dir(child_cwd)
        .env_clear()
        // The VM JIT and regress' regex JIT have independent switches. Set
        // both in the clean child environment before either subsystem can
        // latch its process-global policy.
        .env("ZIPP_NOJIT", "1")
        .env("ZIPP_NO_RX_JIT", "1")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = ChildGuard::new(
        command
            .spawn()
            .map_err(|e| format!("cannot start sandbox worker: {e}"))?,
    );
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or("sandbox worker stdout was not captured")?;
    let stderr = child
        .child
        .stderr
        .take()
        .ok_or("sandbox worker stderr was not captured")?;

    let total = Arc::new(AtomicUsize::new(0));
    let overflow = Arc::new(AtomicBool::new(false));
    let out_thread = drain_bounded(
        stdout,
        config.max_output_bytes,
        Arc::clone(&total),
        Arc::clone(&overflow),
    )?;
    let err_thread = drain_bounded(
        stderr,
        config.max_output_bytes,
        total,
        Arc::clone(&overflow),
    )?;

    let deadline = Duration::from_millis(config.timeout_ms);
    let started = Instant::now();
    let mut stopped_for: Option<&'static str> = None;
    let status = loop {
        if overflow.load(Ordering::Acquire) {
            stopped_for = Some("output limit");
            child.kill();
            break child
                .wait()
                .map_err(|e| format!("cannot reap sandbox worker: {e}"))?;
        }
        if started.elapsed() >= deadline {
            stopped_for = Some("wall-clock timeout");
            child.kill();
            break child
                .wait()
                .map_err(|e| format!("cannot reap sandbox worker: {e}"))?;
        }
        match child
            .try_wait()
            .map_err(|e| format!("cannot inspect sandbox worker: {e}"))?
        {
            Some(status) => break status,
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    };

    let out = out_thread
        .join()
        .map_err(|_| "sandbox stdout reader panicked")?;
    let err = err_thread
        .join()
        .map_err(|_| "sandbox stderr reader panicked")?;
    // A fast child can exit between filling the pipe and the reader flagging
    // overflow. Joining the readers closes that race before success is reported.
    if stopped_for.is_none() && overflow.load(Ordering::Acquire) {
        stopped_for = Some("output limit");
    }
    let out = sanitize_terminal(&out);
    let err = sanitize_terminal(&err);
    std::io::stdout()
        .write_all(&out)
        .map_err(|e| format!("cannot write sandbox stdout: {e}"))?;
    std::io::stderr()
        .write_all(&err)
        .map_err(|e| format!("cannot write sandbox stderr: {e}"))?;

    if let Some(reason) = stopped_for {
        return Err(format!(
            "sandbox stopped the script at its {reason} (timeout={}ms, output={} bytes)",
            config.timeout_ms, config.max_output_bytes
        ));
    }
    if !status.success() {
        return Err(format!("sandboxed script exited with {status}"));
    }
    Ok(())
}

fn print_help() {
    println!("usage: zipp sandbox [options] <file.js>");
    println!();
    println!("Runs an untrusted classic script in a supervised child process.");
    println!("Imports are denied unless --allow-imports is supplied.");
    println!();
    println!("options:");
    println!("  --timeout-ms <n>        hard wall deadline (default {DEFAULT_TIMEOUT_MS})");
    println!("  --max-steps <n>         VM instruction budget (default {DEFAULT_MAX_STEPS})");
    println!("  --max-heap-mb <n>       approximate VM heap cap (default {DEFAULT_MAX_HEAP_MB})");
    println!("  --max-output-bytes <n>  stdout + stderr cap (default {DEFAULT_MAX_OUTPUT_BYTES})");
    println!("  --allow-imports <root>  confine module loading to a canonical directory");
    println!("  --module               unsupported (fails closed; classic scripts only)");
    println!();
    println!("This is process/resource/import containment, not an OS security sandbox.");
}

/// The child half. This is intentionally callable only through a hidden CLI
/// command; all inputs are still re-validated because users can invoke it
/// directly.
pub(crate) fn run_child(args: &[String]) -> Result<(), String> {
    run_child_inner(args).map_err(|error| sanitize_diagnostic(&error))
}

fn run_child_inner(args: &[String]) -> Result<(), String> {
    // Defense in depth for direct invocations of this hidden command: both
    // native-code switches must be set before parsing, compiling, or running
    // anything that could initialize either JIT.
    std::env::set_var("ZIPP_NOJIT", "1");
    std::env::set_var("ZIPP_NO_RX_JIT", "1");

    let config = parse_child(args)?;
    validate_script(&config.script)?;
    let source = read_script(&config.script)?;

    let mut state = zipp_vm::embed::compile_script(&source)?;
    state.disable_vm_jit();
    state.set_limits(config.max_steps, None);
    state.set_dynamic_code_limits(
        MAX_DYNAMIC_SOURCE_BYTES,
        MAX_DYNAMIC_TOTAL_SOURCE_BYTES,
        MAX_DYNAMIC_CALLS,
        MAX_DYNAMIC_FUNCTIONS,
        MAX_DYNAMIC_CLASSES,
    );
    state.set_output_limit(config.max_output_bytes);
    let heap_bytes = config
        .max_heap_mb
        .checked_mul(1024 * 1024)
        .ok_or("--max-heap-mb is too large")?;
    state.set_heap_limit(heap_bytes);

    if let Some(root) = &config.import_root {
        let base = config
            .script
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        state.set_confined_module_loader(base, root, MAX_SOURCE_BYTES)?;
    }

    let result = state.run_init();
    // A guest can turn some failures into rejected promises. The recorder's
    // sticky, typed status is authoritative and must be checked after every
    // guest entry rather than trusting only the direct return value.
    let resource_error = state.resource_limit_error();
    for line in state.take_output() {
        println!("{line}");
    }
    for line in state.take_errput() {
        eprintln!("{line}");
    }
    if let Some(error) = resource_error {
        Err(error.into())
    } else {
        result.map(|_| ())
    }
}

fn parse_public(args: &[String]) -> Result<Config, String> {
    let mut config = Config {
        script: PathBuf::new(),
        import_root: None,
        timeout_ms: DEFAULT_TIMEOUT_MS,
        max_steps: DEFAULT_MAX_STEPS,
        max_heap_mb: DEFAULT_MAX_HEAP_MB,
        max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    };
    let mut i = 0;
    let mut positional = false;
    while i < args.len() {
        let arg = &args[i];
        if !positional && arg == "--" {
            positional = true;
            i += 1;
            continue;
        }
        if !positional && arg.starts_with('-') {
            if arg == "--module" {
                return Err("sandboxed ES-module entry is not supported; sandbox currently accepts classic scripts only".into());
            }
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("{arg} requires a value"))?;
            match arg.as_str() {
                "--timeout-ms" => config.timeout_ms = parse_u64(arg, value)?,
                "--max-steps" => config.max_steps = parse_u64(arg, value)?,
                "--max-heap-mb" => config.max_heap_mb = parse_usize(arg, value)?,
                "--max-output-bytes" => config.max_output_bytes = parse_usize(arg, value)?,
                "--allow-imports" => {
                    config.import_root = Some(canonical_dir(value, "import root")?)
                }
                _ => return Err(format!("unknown sandbox option '{arg}'")),
            }
            i += 2;
            continue;
        }
        if !config.script.as_os_str().is_empty() {
            return Err("sandbox accepts exactly one script path".into());
        }
        config.script = canonical_file(arg, "script")?;
        i += 1;
    }
    validate_config(&config)?;
    Ok(config)
}

fn parse_child(args: &[String]) -> Result<Config, String> {
    let mut config = Config {
        script: PathBuf::new(),
        import_root: None,
        timeout_ms: DEFAULT_TIMEOUT_MS,
        max_steps: 0,
        max_heap_mb: 0,
        max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    };
    let mut i = 0;
    let mut positional = false;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            positional = true;
            i += 1;
            continue;
        }
        if !positional {
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("{arg} requires a value"))?;
            match arg.as_str() {
                "--max-steps" => config.max_steps = parse_u64(arg, value)?,
                "--max-heap-mb" => config.max_heap_mb = parse_usize(arg, value)?,
                "--max-output-bytes" => config.max_output_bytes = parse_usize(arg, value)?,
                "--import-root" => config.import_root = Some(canonical_dir(value, "import root")?),
                _ => return Err(format!("invalid sandbox worker option '{arg}'")),
            }
            i += 2;
            continue;
        }
        if config.script.as_os_str().is_empty() {
            config.script = canonical_file(arg, "script")?;
            i += 1;
            continue;
        }
        return Err(format!("invalid sandbox worker argument '{arg}'"));
    }
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &Config) -> Result<(), String> {
    if config.script.as_os_str().is_empty() {
        return Err("usage: zipp sandbox [limits] [--allow-imports <root>] <file.js>".into());
    }
    if config.timeout_ms == 0 || config.timeout_ms > MAX_TIMEOUT_MS {
        return Err(format!("--timeout-ms must be in 1..={MAX_TIMEOUT_MS}"));
    }
    if config.max_steps == 0 || config.max_steps > i64::MAX as u64 {
        return Err(format!("--max-steps must be in 1..={}", i64::MAX));
    }
    if config.max_heap_mb == 0 || config.max_heap_mb.checked_mul(1024 * 1024).is_none() {
        return Err("--max-heap-mb must be positive and fit in usize bytes".into());
    }
    if config.max_output_bytes == 0 || config.max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(format!(
            "--max-output-bytes must be in 1..={MAX_OUTPUT_BYTES}"
        ));
    }
    if let Some(root) = &config.import_root {
        let base = config.script.parent().unwrap_or_else(|| Path::new("."));
        if !base.starts_with(root) {
            return Err(format!(
                "script directory '{}' is outside --allow-imports root '{}'",
                base.display(),
                root.display()
            ));
        }
    }
    Ok(())
}

fn validate_script(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("cannot inspect sandbox script '{}': {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("sandbox script '{}' is not a file", path.display()));
    }
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "sandbox script is {} bytes; limit is {MAX_SOURCE_BYTES}",
            meta.len()
        ));
    }
    Ok(())
}

fn read_script(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("cannot read sandbox script '{}': {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read sandbox script '{}': {e}", path.display()))?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(format!(
            "sandbox script exceeds the {MAX_SOURCE_BYTES}-byte source limit"
        ));
    }
    String::from_utf8(bytes).map_err(|_| "sandbox script is not valid UTF-8".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForbiddenWindowsPrefix {
    Unc,
    Device,
    VerbatimNetwork,
}

/// Classify Windows path namespaces that must never reach a filesystem API.
///
/// This is deliberately a string-level check instead of `Path::components`:
/// Windows prefixes are otherwise ordinary filename bytes when the same test
/// runs on Unix, and the security property needs a platform-independent unit
/// test that cannot accidentally touch a network share. Mixed slash spellings
/// are accepted by Windows APIs, so both separators are treated alike here.
fn forbidden_windows_prefix(value: &str) -> Option<ForbiddenWindowsPrefix> {
    fn sep(byte: u8) -> bool {
        byte == b'\\' || byte == b'/'
    }

    fn component_eq(input: &[u8], expected: &[u8]) -> bool {
        input.len() >= expected.len()
            && input[..expected.len()].eq_ignore_ascii_case(expected)
            && (input.len() == expected.len() || sep(input[expected.len()]))
    }

    fn verbatim_disk(input: &[u8]) -> bool {
        input.len() >= 3 && input[0].is_ascii_alphabetic() && input[1] == b':' && sep(input[2])
    }

    let bytes = value.as_bytes();
    if bytes.len() >= 2 && sep(bytes[0]) && sep(bytes[1]) {
        if bytes.len() >= 4 && bytes[2] == b'?' && sep(bytes[3]) {
            let tail = &bytes[4..];
            if component_eq(tail, b"UNC") {
                return Some(ForbiddenWindowsPrefix::VerbatimNetwork);
            }
            // `canonicalize` commonly returns this spelling for an ordinary
            // local drive path, and the supervisor passes that absolute path
            // to the child. Keep it usable; reject every other verbatim
            // namespace (`GLOBALROOT`, Volume GUIDs, and similar devices).
            return (!verbatim_disk(tail)).then_some(ForbiddenWindowsPrefix::Device);
        }
        if bytes.len() >= 4 && bytes[2] == b'.' && sep(bytes[3]) {
            return Some(ForbiddenWindowsPrefix::Device);
        }
        return Some(ForbiddenWindowsPrefix::Unc);
    }

    // Native NT namespace spellings are not ordinary UNC paths but can still
    // name devices or redirect into the object manager.
    if !bytes.is_empty() && sep(bytes[0]) {
        let tail = &bytes[1..];
        if component_eq(tail, b"??") || component_eq(tail, b"Device") {
            return Some(ForbiddenWindowsPrefix::Device);
        }
    }
    None
}

fn reject_forbidden_windows_path(value: &str, what: &str) -> Result<(), String> {
    // A leading `//` is a valid (and normally local) absolute spelling on
    // Unix. Keep the classifier portable for pure tests, but enforce these
    // Windows namespace rules only on the platform where they have device or
    // network semantics.
    if !cfg!(windows) {
        return Ok(());
    }
    let Some(prefix) = forbidden_windows_prefix(value) else {
        return Ok(());
    };
    let kind = match prefix {
        ForbiddenWindowsPrefix::Unc => "UNC/network",
        ForbiddenWindowsPrefix::Device => "device namespace",
        ForbiddenWindowsPrefix::VerbatimNetwork => "verbatim network",
    };
    Err(format!(
        "{what} uses a Windows {kind} path, which sandbox does not allow"
    ))
}

fn canonical_file(value: &str, what: &str) -> Result<PathBuf, String> {
    reject_forbidden_windows_path(value, what)?;
    let path = std::fs::canonicalize(value)
        .map_err(|e| format!("cannot resolve {what} '{value}': {e}"))?;
    if !path.is_file() {
        return Err(format!("{what} '{}' is not a file", path.display()));
    }
    Ok(path)
}

fn canonical_dir(value: &str, what: &str) -> Result<PathBuf, String> {
    reject_forbidden_windows_path(value, what)?;
    let path = std::fs::canonicalize(value)
        .map_err(|e| format!("cannot resolve {what} '{value}': {e}"))?;
    if !path.is_dir() {
        return Err(format!("{what} '{}' is not a directory", path.display()));
    }
    Ok(path)
}

fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} expects a positive integer, got '{value}'"))
}

fn parse_usize(name: &str, value: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("{name} expects a positive integer, got '{value}'"))
}

fn unsafe_terminal_format(ch: char) -> bool {
    // Directional overrides/isolates can visually reorder a later status line
    // without being C0/C1 controls. Preserve ordinary Unicode (and emoji ZWJ),
    // but neutralize bidi formatting plus Unicode's extra line separators.
    matches!(
        ch,
        '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{206f}'
    )
}

fn sanitize_text(bytes: &[u8], preserve_layout: bool) -> Vec<u8> {
    fn append(out: &mut Vec<u8>, text: &str, preserve_layout: bool) {
        for ch in text.chars() {
            let allowed_layout = preserve_layout && matches!(ch, '\n' | '\t');
            if (ch.is_control() && !allowed_layout) || unsafe_terminal_format(ch) {
                out.push(b'?');
            } else {
                let mut encoded = [0_u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
            }
        }
    }

    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                append(&mut out, text, preserve_layout);
                break;
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if valid != 0 {
                    append(
                        &mut out,
                        std::str::from_utf8(&rest[..valid]).expect("validated UTF-8 prefix"),
                        preserve_layout,
                    );
                }
                out.push(b'?');
                let invalid = err.error_len().unwrap_or(rest.len() - valid);
                rest = &rest[valid + invalid..];
            }
        }
    }
    out
}

fn sanitize_terminal(bytes: &[u8]) -> Vec<u8> {
    sanitize_text(bytes, true)
}

/// Sanitize a supervisor/argument error as one inert terminal line. Unlike
/// guest stdout/stderr, a diagnostic has no legitimate embedded layout, so
/// newlines and tabs are neutralized alongside ESC, OSC terminators, bidi
/// formatting, C0/C1 controls, and invalid UTF-8.
fn sanitize_diagnostic(error: &str) -> String {
    String::from_utf8(sanitize_text(error.as_bytes(), false))
        .expect("the diagnostic sanitizer always emits UTF-8")
}

fn drain_bounded<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
    total: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<Vec<u8>>, String> {
    std::thread::Builder::new()
        .spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0_u8; 8192];
            loop {
                let n = match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let before = total.fetch_add(n, Ordering::AcqRel);
                let room = limit.saturating_sub(before);
                kept.extend_from_slice(&chunk[..n.min(room)]);
                if n > room {
                    overflow.store(true, Ordering::Release);
                }
            }
            kept
        })
        .map_err(|e| format!("cannot start sandbox output reader: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_network_and_device_prefixes_are_classified_lexically() {
        let cases = [
            (r"\\server\share\entry.js", ForbiddenWindowsPrefix::Unc),
            (r"//server/share/entry.js", ForbiddenWindowsPrefix::Unc),
            (r"\\.\PIPE\zipp", ForbiddenWindowsPrefix::Device),
            (r"//./NUL", ForbiddenWindowsPrefix::Device),
            (
                r"\\?\UNC\server\share\entry.js",
                ForbiddenWindowsPrefix::VerbatimNetwork,
            ),
            (
                r"//?/uNc/server/share/entry.js",
                ForbiddenWindowsPrefix::VerbatimNetwork,
            ),
            (
                r"\\?\GLOBALROOT\Device\HarddiskVolume1",
                ForbiddenWindowsPrefix::Device,
            ),
            (r"\??\C:\entry.js", ForbiddenWindowsPrefix::Device),
            (r"\Device\NamedPipe\zipp", ForbiddenWindowsPrefix::Device),
        ];
        for (path, expected) in cases {
            assert_eq!(forbidden_windows_prefix(path), Some(expected), "{path}");
        }

        for path in [
            r"C:\repo\entry.js",
            r"relative\entry.js",
            r"\rooted\entry.js",
            r"\\?\C:\repo\entry.js",
            "/tmp/repo/entry.js",
        ] {
            assert_eq!(forbidden_windows_prefix(path), None, "{path}");
        }
    }

    #[test]
    fn diagnostic_sanitizer_removes_layout_controls_and_bidi() {
        let input = "prefix\x1b]0;pwn\x07\nline\ttab\u{202e}spoof\u{2069}✓";
        let diagnostic = sanitize_diagnostic(input);
        assert_eq!(diagnostic, "prefix?]0;pwn??line?tab?spoof?✓");
        assert!(!diagnostic.chars().any(char::is_control));
        assert!(!diagnostic.chars().any(unsafe_terminal_format));

        let forwarded = String::from_utf8(sanitize_terminal(input.as_bytes())).unwrap();
        assert_eq!(forwarded, "prefix?]0;pwn?\nline\ttab?spoof?✓");
        assert_eq!(sanitize_text(b"ok\xff\x1b", false), b"ok??");
    }
}
