//! Node's globals, for a program the `zipp` command runs from a file.
//!
//! A program written for Node expects more than ECMAScript: `process`,
//! `global`, `structuredClone`, `queueMicrotask`, timers with ids that
//! `clearTimeout` cancels. They are written in JavaScript (`bootstrap.js`),
//! as Python's runtime is, over a handful of host functions here: the
//! process's environment, arguments and working directory, writes to its
//! output streams, a clock, and exit.
//!
//! None of it costs a program that does not use it. The bootstrap only
//! defines accessors on the global object; the first read of one builds
//! that global and replaces the accessor with it, as a data property a
//! program can assign over.
//!
//! The layer is the command's, switched on per process ([`set_enabled`])
//! for a file run without a harness. An embedder, the WebAssembly engines,
//! the sandbox and the test262 runner (which always passes a harness) get
//! the plain ECMAScript global object.

pub(crate) mod resolve;

use crate::heap::{HeapObj, ObjMap, PropAttr};
use crate::value::Value;
use crate::vm::native::*;
use crate::vm::{Thrown, Vm};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

/// The layer's installer, a function of the host object (see the module
/// docs), and the parts it loads when a global they define is first read.
const BOOTSTRAP: &str = include_str!("bootstrap.js");
const PARTS: &[(&str, &str)] = &[
    ("timers", include_str!("timers.js")),
    ("clone", include_str!("clone.js")),
    ("url_whatwg", include_str!("url_whatwg.js")),
    ("web_events", include_str!("web_events.js")),
    ("process", include_str!("process.js")),
    ("cjs", include_str!("cjs.js")),
    ("builtin/path", include_str!("builtins/path.js")),
    ("builtin/events", include_str!("builtins/events.js")),
    ("builtin/util", include_str!("builtins/util.js")),
    ("builtin/fs", include_str!("builtins/fs.js")),
    ("builtin/os", include_str!("builtins/os.js")),
    ("builtin/tty", include_str!("builtins/tty.js")),
    ("builtin/url", include_str!("builtins/url.js")),
    ("builtin/module", include_str!("builtins/module.js")),
    ("builtin/assert", include_str!("builtins/assert.js")),
    ("builtin/buffer", include_str!("builtins/buffer.js")),
    ("builtin/crypto", include_str!("builtins/crypto.js")),
    ("builtin/stream", include_str!("builtins/stream.js")),
    ("builtin/readline", include_str!("builtins/readline.js")),
    (
        "builtin/string_decoder",
        include_str!("builtins/string_decoder.js"),
    ),
];

static ENABLED: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicBool = AtomicBool::new(false);
static ARGV: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// `process.exitCode` at the end of the run, for the command's exit status.
static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

/// Give programs this process runs Node's globals, with `argv` as
/// `process.argv` (the executable, the script, its arguments).
pub(crate) fn set_enabled(on: bool, argv: Vec<String>) {
    ENABLED.store(on, Ordering::Relaxed);
    if let Ok(mut a) = ARGV.lock() {
        *a = argv;
    }
}

/// Write what programs print as they print it, as `node` does, instead of
/// collecting it for the caller (the command's runs: a long-running
/// program's progress and a prompt before reading input must appear when
/// written; an embedder or a test reads the run's output afterwards).
pub(crate) fn set_live_output(on: bool) {
    LIVE.store(on, Ordering::Relaxed);
}

/// Whether programs this process runs get Node's globals.
pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The exit status the program asked for (`process.exitCode`), 0 if none.
pub(crate) fn exit_code() -> i32 {
    EXIT_CODE.load(Ordering::Relaxed)
}

impl<'p> Vm<'p> {
    /// Define Node's globals, when the process asked for them, before the
    /// program's own declarations. A script also gets CommonJS's `require`,
    /// `module`, `exports`, `__filename` and `__dirname` for itself; a
    /// module does not, as in Node.
    pub(crate) fn install_node_runtime(&mut self, script: bool) -> Result<(), Thrown> {
        if !ENABLED.load(Ordering::Relaxed) || self.node_installed {
            return Ok(());
        }
        self.node_installed = true;
        // The bootstrap is JavaScript the JIT may compile: pin the register
        // file before it runs, as each entry pins it before native code can
        // hold a window pointer (`reserve_jit_regs`; the entry's own call is
        // then a no-op).
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.reserve_jit_regs();
        if LIVE.load(Ordering::Relaxed) && self.console_sink.is_none() {
            self.console_sink = Some(Box::new(|stream, line| {
                use std::io::Write;
                // A closed pipe (`zipp app.ts | head`) ends output, not the run.
                let _ = match stream {
                    crate::vm::ConsoleStream::Stdout => writeln!(std::io::stdout().lock(), "{line}"),
                    crate::vm::ConsoleStream::Stderr => writeln!(std::io::stderr().lock(), "{line}"),
                };
            }));
        }
        let install = self.eval_script(BOOTSTRAP)?;
        let host = self.node_host_object();
        self.node_host = host;
        self.call_value(install, Value::UNDEFINED, &[host, Value::bool(script)])?;
        Ok(())
    }

    /// A module the loader reached that is not an ES module of a file: a
    /// builtin (`node:fs`), or a CommonJS file, evaluated now. Its namespace
    /// has `default` and `module.exports`, both its `module.exports`, and the
    /// names of its own enumerable properties then (Node finds them by
    /// reading the source; these are a superset).
    pub(crate) fn node_import_special(
        &mut self,
        path: &std::path::Path,
    ) -> Result<Option<Value>, Thrown> {
        let Some(text) = path.to_str() else {
            return Ok(None);
        };
        let exports = if let Some(name) = text.strip_prefix("node:") {
            let f = self.node_cjs_fn("builtin")?;
            let name = self.alloc_str(name.to_string());
            self.call_value(f, Value::UNDEFINED, &[name])?
        } else {
            if node_format(path) != "commonjs" {
                return Ok(None);
            }
            let f = self.node_cjs_fn("importCjs")?;
            let name = self.alloc_str(text.to_string());
            self.call_value(f, Value::UNDEFINED, &[name])?
        };
        let keys_fn = self.node_cjs_fn("exportNames")?;
        let keys = self.call_value(keys_fn, Value::UNDEFINED, &[exports])?;
        let n = self.get_prop(keys, "length")?;
        let n = self.to_number_coerce(n)? as usize;
        let mut names: Vec<String> = vec!["default".into(), "module.exports".into()];
        for i in 0..n {
            let k = self.get_prop(keys, &i.to_string())?;
            let k = self.to_js_string(k)?;
            if k != "default" && k != "module.exports" {
                names.push(k);
            }
        }
        let mut entries = Vec::with_capacity(names.len());
        for n in names {
            let v = match n.as_str() {
                "default" | "module.exports" => exports,
                key => self.get_prop(exports, key)?,
            };
            let slot = self.alloc_module_shared_slot()?;
            self.globals[slot as usize] = v;
            self.bump_global_gen(slot);
            entries.push((n, slot));
        }
        let ns = self.alloc_empty_namespace();
        self.populate_module_namespace(ns, &entries);
        let ns = Value::heap(ns);
        self.module_cache.insert(path.to_path_buf(), ns);
        Ok(Some(ns))
    }

    /// A function of the CommonJS loader part (`cjs.js`), loaded once.
    fn node_cjs_fn(&mut self, name: &str) -> Result<Value, Thrown> {
        if !self.node_cjs.is_heap() {
            let src = PARTS
                .iter()
                .find(|(n, _)| *n == "cjs")
                .map(|(_, s)| *s)
                .unwrap_or("");
            let make = self.eval_script(src)?;
            let host = self.node_host;
            self.node_cjs = self.call_value(make, Value::UNDEFINED, &[host])?;
        }
        let cjs = self.node_cjs;
        if name == "self" {
            return Ok(cjs);
        }
        self.get_prop(cjs, name)
    }

    /// Bytes as a `Uint8Array` (the JavaScript side makes it a `Buffer`).
    fn node_bytes(&mut self, bytes: Vec<u8>) -> Result<Value, Thrown> {
        let buf = self.heap.alloc(HeapObj::ArrayBuffer {
            data: bytes.into(),
            detached: false,
        });
        if self.arraybuffer_proto != 0 {
            self.proto_of
                .insert(buf, Value::heap(self.arraybuffer_proto));
        }
        self.build_typed_array(1, &[Value::heap(buf)])
    }

    /// The file system operations `fs` is built on: `op` on `path`, `a` and
    /// `b` its operands.
    fn node_fs(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let op = self.to_js_string(arg(0))?;
        let path = self.to_js_string(arg(1))?;
        let p = std::path::Path::new(&path);
        Ok(match op.as_str() {
            "stat" | "lstat" => {
                let meta = if op == "lstat" {
                    std::fs::symlink_metadata(p)
                } else {
                    std::fs::metadata(p)
                };
                match meta {
                    Ok(m) => {
                        let mut map = ObjMap::new();
                        let kind = if m.is_dir() {
                            "dir"
                        } else if m.file_type().is_symlink() {
                            "link"
                        } else {
                            "file"
                        };
                        let kind = self.alloc_str(kind.to_string());
                        map.define("kind", kind, PropAttr::data());
                        map.define("size", Value::num(m.len() as f64), PropAttr::data());
                        let ms = |t: std::io::Result<std::time::SystemTime>| {
                            t.ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
                        };
                        map.define("mtimeMs", Value::num(ms(m.modified())), PropAttr::data());
                        map.define("atimeMs", Value::num(ms(m.accessed())), PropAttr::data());
                        map.define("birthtimeMs", Value::num(ms(m.created())), PropAttr::data());
                        map.define(
                            "readonly",
                            Value::bool(m.permissions().readonly()),
                            PropAttr::data(),
                        );
                        self.alloc_object_current_realm(map)
                    }
                    Err(e) if self.truthy(arg(2)) => {
                        // `throwIfNoEntry: false`.
                        let _ = e;
                        Value::UNDEFINED
                    }
                    Err(e) => return Err(io_error(&e, &op, &path)),
                }
            }
            "readdir" => {
                let mut names: Vec<String> = std::fs::read_dir(p)
                    .map_err(|e| io_error(&e, "scandir", &path))?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                let items: Vec<Value> = names.into_iter().map(|n| self.alloc_str(n)).collect();
                self.alloc_array_current_realm(items)
            }
            "write" | "append" => {
                use std::io::Write;
                let data: Vec<u8> = match self.buffer_source_bytes(arg(2)) {
                    Ok(bytes) => bytes,
                    Err(_) => self.to_js_string(arg(2))?.into_bytes(),
                };
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(op == "append")
                    .truncate(op == "write")
                    .open(p)
                    .map_err(|e| io_error(&e, "open", &path))?;
                f.write_all(&data)
                    .map_err(|e| io_error(&e, "write", &path))?;
                Value::UNDEFINED
            }
            "mkdir" => {
                let r = if self.truthy(arg(2)) {
                    std::fs::create_dir_all(p)
                } else {
                    std::fs::create_dir(p)
                };
                r.map_err(|e| io_error(&e, "mkdir", &path))?;
                Value::UNDEFINED
            }
            "rm" => {
                let recursive = self.truthy(arg(2));
                let r = match std::fs::symlink_metadata(p) {
                    Ok(m) if m.is_dir() && recursive => std::fs::remove_dir_all(p),
                    Ok(m) if m.is_dir() => std::fs::remove_dir(p),
                    Ok(_) => std::fs::remove_file(p),
                    Err(e) if self.truthy(arg(3)) => {
                        // `force`: a missing path is no error.
                        let _ = e;
                        Ok(())
                    }
                    Err(e) => Err(e),
                };
                r.map_err(|e| io_error(&e, "rm", &path))?;
                Value::UNDEFINED
            }
            "rename" => {
                let to = self.to_js_string(arg(2))?;
                std::fs::rename(p, &to).map_err(|e| io_error(&e, "rename", &path))?;
                Value::UNDEFINED
            }
            "copy" => {
                let to = self.to_js_string(arg(2))?;
                std::fs::copy(p, &to).map_err(|e| io_error(&e, "copyfile", &path))?;
                Value::UNDEFINED
            }
            "realpath" => {
                let real = std::fs::canonicalize(p).map_err(|e| io_error(&e, "realpath", &path))?;
                self.alloc_str(strip_verbatim(&real))
            }
            _ => return Err(Thrown(format!("Error: unsupported file operation {op}"))),
        })
    }

    /// The host functions the bootstrap is handed.
    fn node_host_object(&mut self) -> Value {
        let mut m = ObjMap::new();
        let attr = PropAttr::data();
        for (name, id) in [
            ("write", NODE_HOST_WRITE),
            ("env", NODE_HOST_ENV),
            ("argv", NODE_HOST_ARGV),
            ("cwd", NODE_HOST_CWD),
            ("exit", NODE_HOST_EXIT),
            ("hrtime", NODE_HOST_HRTIME),
            ("isatty", NODE_HOST_ISATTY),
            ("onExit", NODE_HOST_ON_EXIT),
            ("onTicks", NODE_HOST_ON_TICKS),
            ("requestTicks", NODE_HOST_REQUEST_TICKS),
            ("load", NODE_HOST_LOAD),
            ("readFile", NODE_HOST_READ_FILE),
            ("resolve", NODE_HOST_RESOLVE),
            ("compileCjs", NODE_HOST_COMPILE_CJS),
            ("importEsm", NODE_HOST_IMPORT_ESM),
            ("format", NODE_HOST_FORMAT),
            ("fs", NODE_HOST_FS),
            ("cjs", NODE_HOST_CJS),
            ("inspect", NODE_HOST_INSPECT),
            ("readStdin", NODE_HOST_READ_STDIN),
            ("timer", NODE_HOST_TIMER),
        ] {
            let f = Value::heap(self.heap.alloc(HeapObj::Native(id)));
            m.define(name, f, attr);
        }
        let platform = self.alloc_str(
            match std::env::consts::OS {
                "windows" => "win32",
                "macos" => "darwin",
                other => other,
            }
            .to_string(),
        );
        m.define("platform", platform, attr);
        let arch = self.alloc_str(
            match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                "x86" => "ia32",
                other => other,
            }
            .to_string(),
        );
        m.define("arch", arch, attr);
        m.define("pid", Value::num(std::process::id() as f64), attr);
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "zipp".into());
        let exe = self.alloc_str(exe);
        m.define("execPath", exe, attr);
        let version = self.alloc_str(env!("CARGO_PKG_VERSION").to_string());
        m.define("zippVersion", version, attr);
        self.alloc_object_current_realm(m)
    }

    /// The host functions' bodies.
    pub(crate) fn node_host_native(&mut self, id: u16, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        Ok(match id {
            NODE_HOST_WRITE => {
                // `process.stdout.write(text)`: whole lines go out as console
                // lines; a partial one waits for the rest of its line (or the
                // next `console.log`, which it begins).
                let to_stderr = self.to_number_coerce(arg(0))? == 2.0;
                let text = self.to_js_string(arg(1))?;
                self.node_write(&text, to_stderr);
                Value::bool(true)
            }
            NODE_HOST_ENV => {
                let mut m = ObjMap::new();
                for (k, v) in std::env::vars_os() {
                    let (Some(k), Some(v)) = (k.to_str(), v.to_str()) else {
                        continue;
                    };
                    let v = self.alloc_str(v.to_string());
                    m.define(k, v, PropAttr::data());
                }
                self.alloc_object_current_realm(m)
            }
            NODE_HOST_ARGV => {
                let argv = ARGV.lock().map(|a| a.clone()).unwrap_or_default();
                let items: Vec<Value> = argv.into_iter().map(|s| self.alloc_str(s)).collect();
                self.alloc_array_current_realm(items)
            }
            NODE_HOST_CWD => {
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.alloc_str(cwd)
            }
            NODE_HOST_EXIT => {
                let code = self.to_number_coerce(arg(0))?;
                let code = if code.is_finite() { code as i32 } else { 0 };
                self.node_exit(code)
            }
            NODE_HOST_HRTIME => {
                // `[seconds, nanoseconds]` since a fixed point of this process.
                // A difference, which the wasm `Instant` has as well (it has no
                // `duration_since`); both saturate at zero.
                let d = crate::vm::clock::Instant::now() - node_epoch();
                let items = vec![
                    Value::num(d.as_secs() as f64),
                    Value::num(d.subsec_nanos() as f64),
                ];
                self.alloc_array_current_realm(items)
            }
            NODE_HOST_ISATTY => {
                use std::io::IsTerminal;
                let fd = self.to_number_coerce(arg(0))?;
                Value::bool(match fd as i64 {
                    0 => std::io::stdin().is_terminal(),
                    1 => std::io::stdout().is_terminal(),
                    2 => std::io::stderr().is_terminal(),
                    _ => false,
                })
            }
            NODE_HOST_ON_EXIT => {
                // The function the run's end calls (`process`'s `exit` event
                // and `exitCode`).
                self.node_exit_hook = arg(0);
                Value::UNDEFINED
            }
            NODE_HOST_ON_TICKS => {
                self.node_tick_hook = arg(0);
                Value::UNDEFINED
            }
            NODE_HOST_REQUEST_TICKS => {
                self.node_tick_pending = true;
                Value::UNDEFINED
            }
            NODE_HOST_TIMER => {
                // `timer(callback, op)` on the native timeout armed with
                // `callback`: 0 cancels it (`clearTimeout`), 1 refs it, 2
                // unrefs it (an unref'd timeout runs if the loop is still
                // going, but does not keep it going).
                let cb = arg(0);
                match self.to_number(arg(1))? as i32 {
                    0 => {
                        self.timer_queue.retain(|&(_, c)| c.bits() != cb.bits());
                        self.timer_unref.remove(&cb.bits());
                    }
                    1 => {
                        self.timer_unref.remove(&cb.bits());
                    }
                    _ => {
                        if self.timer_queue.iter().any(|&(_, c)| c.bits() == cb.bits()) {
                            self.timer_unref.insert(cb.bits());
                        }
                    }
                }
                Value::UNDEFINED
            }
            NODE_HOST_LOAD => {
                // A part of the layer, compiled now: the function its source
                // is.
                let name = self.to_js_string(arg(0))?;
                let Some(&(_, src)) = PARTS.iter().find(|(n, _)| *n == name) else {
                    return Err(Thrown(format!(
                        "Error: no such part of Node's globals: {name}"
                    )));
                };
                self.eval_script(src)?
            }
            NODE_HOST_CJS => {
                // The CommonJS loader: one for the main script's `require`
                // and the module loader's imports of CommonJS files.
                self.node_cjs_fn("self")?
            }
            NODE_HOST_READ_STDIN => {
                // `readStdin(max)`: up to `max` bytes as they arrive (a
                // terminal hands over a line at a time), or all of it when
                // `max` is negative; null at the end of input.
                use std::io::Read;
                let max = self.to_number_coerce(arg(0))?;
                let mut stdin = std::io::stdin().lock();
                let mut bytes = Vec::new();
                if max < 0.0 {
                    stdin
                        .read_to_end(&mut bytes)
                        .map_err(|e| io_error(&e, "read", "<stdin>"))?;
                    let text = String::from_utf8(bytes)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                    return Ok(self.alloc_str(text));
                }
                bytes.resize((max as usize).clamp(1, 1 << 20), 0);
                let n = stdin
                    .read(&mut bytes)
                    .map_err(|e| io_error(&e, "read", "<stdin>"))?;
                if n == 0 {
                    return Ok(Value::NULL);
                }
                bytes.truncate(n);
                // The bytes as read (a stream decodes them, keeping a
                // character split between two reads whole).
                if self.truthy(arg(1)) {
                    return self.node_bytes(bytes);
                }
                let text = String::from_utf8(bytes)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                self.alloc_str(text)
            }
            NODE_HOST_READ_FILE => {
                let path = self.to_js_string(arg(0))?;
                let bytes = std::fs::read(&path).map_err(|e| io_error(&e, "open", &path))?;
                if self.truthy(arg(1)) {
                    let text = String::from_utf8(bytes)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                    self.alloc_str(text)
                } else {
                    self.node_bytes(bytes)?
                }
            }
            NODE_HOST_RESOLVE => {
                // `require(spec)` from folder `dir`: the file, or null.
                let spec = self.to_js_string(arg(0))?;
                let dir = self.to_js_string(arg(1))?;
                let dir = std::path::Path::new(&dir);
                let found = if resolve::is_bare(&spec) {
                    resolve::resolve_package(&spec, dir, true)
                } else {
                    let p = dir.join(&spec);
                    resolve::probe(&p).or_else(|| {
                        crate::lang::ts::resolve_candidates(&p)
                            .into_iter()
                            .find(|c| c.is_file())
                    })
                };
                match found.map(|p| std::fs::canonicalize(&p).unwrap_or(p)) {
                    Some(p) => self.alloc_str(strip_verbatim(&p)),
                    None => Value::NULL,
                }
            }
            NODE_HOST_COMPILE_CJS => {
                // A CommonJS file's source, inside Node's wrapper function.
                let src = self.to_js_string(arg(0))?;
                let body = match src.strip_prefix("#!") {
                    Some(rest) => format!("//{rest}"),
                    None => src,
                };
                let body = body.strip_prefix('\u{feff}').unwrap_or(&body);
                let wrapped = format!(
                    "(function (exports, require, module, __filename, __dirname) {{{body}\n}})"
                );
                // A syntax error names the file it is in.
                match self.eval_script(&wrapped) {
                    Ok(f) => f,
                    Err(Thrown(msg)) if msg.starts_with("SyntaxError") => {
                        let file = self.to_js_string(arg(1))?;
                        return Err(Thrown(format!("{msg} (in {file})")));
                    }
                    Err(t) => return Err(t),
                }
            }
            NODE_HOST_IMPORT_ESM => {
                // `require(esm)`: the module's namespace.
                let path = self.to_js_string(arg(0))?;
                self.import_module(std::path::Path::new(&path), None)?
            }
            NODE_HOST_FORMAT => {
                let path = self.to_js_string(arg(0))?;
                let f = node_format(std::path::Path::new(&path));
                self.alloc_str(f.to_string())
            }
            NODE_HOST_FS => self.node_fs(args)?,
            NODE_HOST_INSPECT => {
                // `util.inspect`: `console.log`'s rendering of one value.
                let v = arg(0);
                let line = self.inspect_line(1, |_| v);
                self.alloc_str(line)
            }
            _ => Value::UNDEFINED,
        })
    }

    /// Run `process.nextTick`'s queue. A throw ends the program as an
    /// uncaught one does, after the job it ran in.
    pub(crate) fn node_run_ticks(&mut self) {
        self.node_tick_pending = false;
        let hook = self.node_tick_hook;
        if !self.is_callable(hook) {
            return;
        }
        if let Err(Thrown(msg)) = self.call_value(hook, Value::UNDEFINED, &[]) {
            let v = match self.pending_throw.take() {
                Some(v) => v,
                None => self.error_from_thrown(&msg),
            };
            if self.uncaught_timer_throw.is_none() {
                self.uncaught_timer_throw = Some(v);
            }
        }
    }

    /// Text written to a stream: complete lines become console lines, a
    /// trailing partial one is kept for what follows.
    fn node_write(&mut self, text: &str, to_stderr: bool) {
        // Written as it is, partial line and all: a prompt is on screen
        // before the read that waits for its answer.
        if LIVE.load(Ordering::Relaxed) && self.console_sink.is_some() {
            use std::io::Write;
            let _ = if to_stderr {
                let mut e = std::io::stderr().lock();
                e.write_all(text.as_bytes()).and_then(|_| e.flush())
            } else {
                let mut o = std::io::stdout().lock();
                o.write_all(text.as_bytes()).and_then(|_| o.flush())
            };
            return;
        }
        let i = to_stderr as usize;
        let mut buf = std::mem::take(&mut self.node_partial[i]);
        buf.push_str(text);
        let mut rest = buf.as_str();
        while let Some(n) = rest.find('\n') {
            let line = rest[..n]
                .strip_suffix('\r')
                .unwrap_or(&rest[..n])
                .to_string();
            self.push_console_line_raw(line, to_stderr);
            rest = &rest[n + 1..];
        }
        self.node_partial[i] = rest.to_string();
    }

    /// The partial lines `process.stdout.write` left, as lines of their own
    /// once nothing follows them.
    pub(crate) fn node_flush_partial(&mut self) {
        for i in 0..2 {
            let rest = std::mem::take(&mut self.node_partial[i]);
            if !rest.is_empty() {
                self.push_console_line_raw(rest, i == 1);
            }
        }
    }

    /// The end of the run: the program's `exit` listeners, and the status it
    /// asked for.
    pub(crate) fn node_run_ended(&mut self) {
        if !self.node_installed {
            return;
        }
        let hook = std::mem::replace(&mut self.node_exit_hook, Value::UNDEFINED);
        if self.is_callable(hook) {
            if let Ok(code) = self.call_value(hook, Value::UNDEFINED, &[]) {
                if let Ok(c) = self.to_number_coerce(code) {
                    if c.is_finite() {
                        EXIT_CODE.store(c as i32, Ordering::Relaxed);
                    }
                }
            }
        }
        self.node_flush_partial();
    }

    /// `process.exit(code)`: the output so far is written as the command
    /// would write it at the end, and the process ends.
    fn node_exit(&mut self, code: i32) -> ! {
        use std::io::Write;
        self.node_flush_partial();
        let mut out = std::io::stdout().lock();
        for line in std::mem::take(&mut self.output) {
            let _ = writeln!(out, "{line}");
        }
        let _ = out.flush();
        let mut err = std::io::stderr().lock();
        for line in std::mem::take(&mut self.errput) {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
        std::process::exit(code)
    }
}

/// `\\?\C:\x` (what `canonicalize` gives on Windows) as `C:\x`.
fn strip_verbatim(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_string(),
        _ => s.into_owned(),
    }
}

/// A file's module format, as Node decides it: `.cjs` and `.mjs` by their
/// names, `.json`, TypeScript as a module, and a `.js` by its package's
/// `"type"`; with none, by whether its source has `import`/`export`
/// statements (Node's syntax detection).
pub(crate) fn node_format(path: &std::path::Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "cjs" => "commonjs",
        "mjs" | "ts" | "mts" | "cts" | "tsx" | "jsx" => "module",
        "json" => "json",
        _ => {
            let dir = path.parent().unwrap_or(std::path::Path::new("."));
            let declared = resolve::nearest_package(dir).and_then(|(_, p)| p.module_type.clone());
            match declared.as_deref() {
                Some("module") => "module",
                Some(_) => "commonjs",
                None => match std::fs::read_to_string(path) {
                    Ok(src) if crate::lang::ts::looks_like_module(&src) => "module",
                    _ => "commonjs",
                },
            }
        }
    }
}

/// An I/O error as Node words it (`ENOENT: no such file or directory, open
/// 'x'`), its code first for the JavaScript side to read.
fn io_error(e: &std::io::Error, syscall: &str, path: &str) -> Thrown {
    use std::io::ErrorKind as K;
    let (code, what) = match e.kind() {
        K::NotFound => ("ENOENT", "no such file or directory"),
        K::PermissionDenied => ("EACCES", "permission denied"),
        K::AlreadyExists => ("EEXIST", "file already exists"),
        K::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        K::NotADirectory => ("ENOTDIR", "not a directory"),
        K::DirectoryNotEmpty => ("ENOTEMPTY", "directory not empty"),
        _ => ("EIO", "i/o error"),
    };
    Thrown(format!("Error: {code}: {what}, {syscall} '{path}'"))
}

/// The process's clock origin for `process.hrtime`.
fn node_epoch() -> crate::vm::clock::Instant {
    static EPOCH: std::sync::OnceLock<crate::vm::clock::Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(crate::vm::clock::Instant::now)
}
