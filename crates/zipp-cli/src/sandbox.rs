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
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }
    let config = parse_public(args)?;
    validate_script(&config.script)?;

    let exe =
        std::env::current_exe().map_err(|e| format!("cannot locate the zipp executable: {e}"))?;
    let cwd = config
        .script
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut command = Command::new(exe);
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
        .current_dir(cwd)
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
    for line in state.take_output() {
        println!("{line}");
    }
    for line in state.take_errput() {
        eprintln!("{line}");
    }
    result.map(|_| ())
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

fn canonical_file(value: &str, what: &str) -> Result<PathBuf, String> {
    let path = std::fs::canonicalize(value)
        .map_err(|e| format!("cannot resolve {what} '{value}': {e}"))?;
    if !path.is_file() {
        return Err(format!("{what} '{}' is not a file", path.display()));
    }
    Ok(path)
}

fn canonical_dir(value: &str, what: &str) -> Result<PathBuf, String> {
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

fn sanitize_terminal(bytes: &[u8]) -> Vec<u8> {
    fn unsafe_format(ch: char) -> bool {
        // Directional overrides/isolates can visually reorder a later status
        // line without being C0/C1 controls. Preserve ordinary Unicode (and
        // emoji ZWJ), but neutralize the formatting characters used in bidi
        // terminal/log spoofing plus Unicode's extra line separators.
        matches!(
            ch,
            '\u{061c}'
                | '\u{200e}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
    }

    fn append(out: &mut Vec<u8>, text: &str) {
        for ch in text.chars() {
            if (ch.is_control() && ch != '\n' && ch != '\t') || unsafe_format(ch) {
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
                append(&mut out, text);
                break;
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if valid != 0 {
                    append(
                        &mut out,
                        std::str::from_utf8(&rest[..valid]).expect("validated UTF-8 prefix"),
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
