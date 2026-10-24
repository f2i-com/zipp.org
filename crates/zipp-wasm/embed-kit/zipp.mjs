// zipp.mjs: load and sandbox ZIPP WebAssembly engines from their .wasm files.
//
//   import { loadZipp } from "./zipp.mjs";
//   const zipp = await loadZipp("./zipp-0.0.26-javascript.wasm");
//   const sandbox = await zipp.createSandbox();          // nothing permitted
//   const { output } = await sandbox.run("console.log(6 * 7)");   // ["42"]
//   await sandbox.close();
//
// Every engine module a ZIPP release publishes carries its own JavaScript
// bindings in a custom section, `zipp.glue`, beside a `zipp.meta` record of
// its version, commit and variant. This loader compiles the module, imports
// those bindings and initializes them with it, so one file runs every
// variant of every release and the bindings can never belong to another
// build. Add-on modules (the torch package) are loaded with it:
//
//   await loadZipp({ engine: "zipp-0.0.26-javascript-python-base.wasm",
//                    packages: ["zipp-0.0.26-torch-package.wasm"] });
//
// A sandbox starts with no storage, no files, no network and no host
// functions; `permissions` grants each one explicitly, `limits` bounds the
// steps and the time a program may take, and by default it runs in its own
// Worker (a worker thread in Node), which a timeout terminates. See
// README.md beside this file.
//
// Browsers, Workers, Deno and Bun import the bindings from a `blob:` URL,
// Node from a `data:` URL; a Content-Security-Policy must allow one of them
// in `script-src`. One file, no dependencies: copy it anywhere.

export const ZIPP_LOADER_VERSION = "1.3.0";

const GLUE_SECTION = "zipp.glue";
const META_SECTION = "zipp.meta";
// The record format this loader reads; a newer module refuses to load
// rather than being half understood.
const META_FORMAT = 1;
const WORKER_NAME = "zipp-sandbox";

function nodeLike() {
  return typeof process !== "undefined" && typeof process.versions?.node === "string";
}

// A source Node should read from disk: a file: URL, an absolute or relative
// path, or a Windows drive path. Anything with another scheme is fetched.
function localPath(source) {
  if (!nodeLike()) return null;
  if (source instanceof URL) return source.protocol === "file:" ? source : null;
  if (/^[a-zA-Z]:[\\/]/.test(source)) return source;
  if (/^file:/i.test(source)) return new URL(source);
  return /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(source) ? null : source;
}

// A module starts with "\0asm"; anything else handed to the loader may be the
// Brotli-compressed `.wasm.br` a release also publishes.
function isWasm(bytes) {
  const b = new Uint8Array(bytes.buffer ?? bytes, bytes.byteOffset ?? 0, Math.min(4, bytes.byteLength));
  return b.length === 4 && b[0] === 0 && b[1] === 0x61 && b[2] === 0x73 && b[3] === 0x6d;
}

// Decompress Brotli where the runtime can: Node's zlib, or a
// DecompressionStream("brotli") (Firefox, Node). Null where it cannot
// (Chromium and WebKit, as of October 2026).
async function brotliDecode(bytes) {
  if (nodeLike()) {
    try {
      const zlib = await import("node:zlib");
      return new Uint8Array(zlib.brotliDecompressSync(bytes));
    } catch {
      return null;
    }
  }
  if (typeof DecompressionStream === "function" && typeof Blob === "function") {
    try {
      const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("brotli"));
      return new Uint8Array(await new Response(stream).arrayBuffer());
    } catch {
      return null;
    }
  }
  return null;
}

// Compile module bytes, decompressing them first when they are Brotli.
async function compileBytes(bytes) {
  if (!isWasm(bytes)) {
    const decoded = await brotliDecode(bytes);
    if (decoded && isWasm(decoded)) return WebAssembly.compile(decoded);
  }
  return WebAssembly.compile(bytes);
}

const brotliUrl = url => /\.br$/i.test(url.pathname);

async function compileResponse(response, url) {
  if (!response.ok) {
    throw new Error(`zipp: fetching ${response.url || url || "the module"} failed with HTTP ${response.status}`);
  }
  // Streaming compilation needs `Content-Type: application/wasm` (and a body
  // the browser has already decoded); GitHub release downloads and many
  // static hosts send octet-stream instead.
  const type = (response.headers.get("content-type") || "").split(";")[0].trim();
  if (type === "application/wasm" && typeof WebAssembly.compileStreaming === "function") {
    try {
      return await WebAssembly.compileStreaming(response.clone());
    } catch {
      // Fall back to the buffered path below.
    }
  }
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (isWasm(bytes)) return WebAssembly.compile(bytes);
  // A `.wasm.br` the server sent without `Content-Encoding: br`: decompress
  // it here where the runtime can, or else fetch the plain `.wasm` beside it.
  const where = url ?? (response.url ? new URL(response.url) : null);
  if (where && brotliUrl(where)) {
    const decoded = await brotliDecode(bytes);
    if (decoded && isWasm(decoded)) return WebAssembly.compile(decoded);
    const plain = new URL(where.href.replace(/\.br(?=$|[?#])/i, ""));
    const fallback = await fetch(plain);
    if (fallback.ok) return compileResponse(fallback, plain);
    throw new Error(`zipp: ${where.href} is Brotli-compressed and was not decoded, and ${plain.href} is missing: serve the .br with Content-Encoding: br, or put the plain .wasm beside it`);
  }
  return compileBytes(bytes);
}

/**
 * Compile a ZIPP module from a URL or path, a `Response` (or a promise of
 * one), bytes, a `Blob`, or an existing `WebAssembly.Module`. A Brotli
 * `.wasm.br` works in every form: decoded by the server
 * (`Content-Encoding: br`), decompressed here where the runtime can, or, for
 * a URL, replaced by the plain `.wasm` beside it.
 */
export async function compileZipp(source) {
  source = await source;
  if (source instanceof WebAssembly.Module) return source;
  if (typeof Response !== "undefined" && source instanceof Response) return compileResponse(source);
  if (typeof source === "string" || source instanceof URL) {
    const file = localPath(source);
    if (file) {
      const { readFile } = await import("node:fs/promises");
      return compileBytes(new Uint8Array(await readFile(file)));
    }
    const base = typeof document !== "undefined" ? document.baseURI : globalThis.location?.href;
    const url = base ? new URL(source, base) : new URL(source);
    return compileResponse(await fetch(url), url);
  }
  if (typeof Blob !== "undefined" && source instanceof Blob) {
    return compileBytes(new Uint8Array(await source.arrayBuffer()));
  }
  if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) return compileBytes(source);
  throw new TypeError("zipp: expected a URL, path, Response, bytes, Blob or WebAssembly.Module");
}

function section(module, name) {
  const found = WebAssembly.Module.customSections(module, name);
  if (found.length > 1) throw new Error(`zipp: the module has ${found.length} ${name} sections`);
  return found.length ? new TextDecoder().decode(found[0]) : null;
}

/**
 * The `zipp.meta` record of a compiled module (version, commit, variant,
 * languages), or `null` for a module that is not from a ZIPP release.
 * Reading it runs nothing.
 */
export function readZippMeta(module) {
  const text = section(module, META_SECTION);
  return text === null ? null : JSON.parse(text);
}

function base64(text) {
  if (typeof Buffer === "function") return Buffer.from(text, "utf8").toString("base64");
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

let instances = 0;

async function importBindings(glue) {
  // A distinct URL per load: a module cache keyed by URL would otherwise hand
  // a second load the first one's bindings, and with them its instance.
  const text = `${glue}\n// zipp instance ${++instances}\n`;
  let blobError = null;
  if (!nodeLike() && typeof Blob !== "undefined" && typeof URL.createObjectURL === "function") {
    const url = URL.createObjectURL(new Blob([text], { type: "text/javascript" }));
    try {
      return await import(/* webpackIgnore: true */ /* @vite-ignore */ url);
    } catch (error) {
      blobError = error;
    } finally {
      URL.revokeObjectURL(url);
    }
  }
  try {
    return await import(/* webpackIgnore: true */ /* @vite-ignore */ `data:text/javascript;base64,${base64(text)}`);
  } catch (error) {
    const why = blobError ? `blob: (${blobError.message}) or data: (${error.message})` : `data: (${error.message})`;
    throw new Error(`zipp: could not import the module's bindings from ${why}; a Content-Security-Policy must allow blob: or data: scripts`);
  }
}

// The torch package (zipp-<version>-torch-package.wasm) for a
// javascript-python-base engine: the same steps as zipp_torch.js.
function installTorch(bindings, instance) {
  const x = instance.exports;
  const archive = new Uint8Array(x.memory.buffer, x.zipp_package_ptr(), x.zipp_package_len()).slice();
  const kernels = {
    zippTorchKernel(request) {
      const ptr = x.zipp_alloc(request.length);
      new Uint8Array(x.memory.buffer, ptr, request.length).set(request);
      const out = x.zipp_kernel(ptr, request.length);
      const len = new DataView(x.memory.buffer).getUint32(out, true);
      const response = new Uint8Array(x.memory.buffer, out + 4, len).slice();
      x.zipp_free(out, len + 4);
      return response;
    },
  };
  return JSON.parse(bindings.addPythonPackage(archive, kernels));
}

function asError(value) {
  return value instanceof Error ? value : new Error(String(value));
}

// Both drains page: each call returns what fits one conversion, until empty.
function drain(take, text) {
  const lines = [];
  for (;;) {
    const page = take();
    if (!Array.isArray(page) || page.length === 0) return lines;
    for (const entry of page) lines.push(text(entry));
  }
}

// Compile, check and instantiate an engine module and its packages: the part
// a sandbox's Worker repeats with the same compiled modules.
async function instantiate(engineModule, packageModules) {
  const meta = readZippMeta(engineModule);
  if (!meta) throw new Error("zipp: this .wasm carries no zipp.meta record; use a module from a ZIPP release (0.0.26 or later)");
  if (meta.format !== META_FORMAT) {
    throw new Error(`zipp: module record format ${meta.format} is not ${META_FORMAT}; use the loader from the same release`);
  }
  if (meta.variant === "torch-package") {
    throw new Error("zipp: this is the torch package, not an engine; load it as one of an engine's `packages`");
  }
  const glue = section(engineModule, GLUE_SECTION);
  if (glue === null) throw new Error("zipp: this .wasm carries no bindings (zipp.glue section)");
  const bindings = await importBindings(glue);
  await bindings.default({ module_or_path: engineModule });
  const profile = JSON.parse(bindings.zippProfile());
  // The record and the engine must name the same build.
  if (profile.version !== meta.version || (profile.source?.sha ?? null) !== meta.commit) {
    throw new Error(`zipp: the module reports ${profile.version} at ${profile.source?.sha}, but its record says ${meta.version} at ${meta.commit}`);
  }
  for (const pkg of packageModules) {
    const pkgMeta = readZippMeta(pkg);
    if (pkgMeta?.variant !== "torch-package" || pkgMeta.version !== meta.version) {
      throw new Error(`zipp: a package must be the ${meta.version} torch package, not ${pkgMeta ? `${pkgMeta.variant} ${pkgMeta.version}` : "an unrecorded module"}`);
    }
    if (typeof bindings.addPythonPackage !== "function") {
      throw new Error(`zipp: the ${meta.variant} engine has no Python to add packages to`);
    }
    installTorch(bindings, await WebAssembly.instantiate(pkg, {}));
  }
  return { meta, profile, bindings };
}

/**
 * Load a ZIPP engine: a module source (see compileZipp), or
 * `{ engine, packages }` to add the torch package to a
 * javascript-python-base engine in the same step. Resolves to an object
 * with `createSandbox(options)`, a one-shot `run(code)`, the engine's
 * `Engine` class and `profile`, the module's `meta` record and the raw
 * `bindings`.
 */
export async function loadZipp(source, options = {}) {
  let engineSource = source;
  let packageSources = options.packages ?? [];
  if (source && typeof source === "object" && !(source instanceof URL) && "engine" in source) {
    engineSource = source.engine;
    packageSources = source.packages ?? [];
  }
  const module = await compileZipp(engineSource);
  const packages = await Promise.all([...packageSources].map(compileZipp));
  const { meta, profile, bindings } = await instantiate(module, packages);
  if (options.expectVersion && meta.version !== options.expectVersion) {
    throw new Error(`zipp: expected ZIPP ${options.expectVersion}, got ${meta.version}`);
  }
  const hasPython = typeof bindings.addPythonPackage === "function";
  const zipp = {
    meta,
    profile,
    module,
    packages: Object.freeze(packages),
    bindings,
    Engine: bindings.Engine,
    languages: Object.freeze([...profile.languages]),

    /**
     * Run `code` in a fresh engine, here and now, and return `{ output }`.
     * Nothing is permitted and nothing outside the program runs; a failure
     * throws, with what was printed before it as `error.output`. For
     * permissions, limits on time, or Worker isolation, use createSandbox.
     */
    run(code, { language = "javascript", instructionBudget = null } = {}) {
      const engine = new bindings.Engine();
      try {
        engine.setSyncHostCapabilities([]);
        if (instructionBudget != null) engine.setInstructionBudget(instructionBudget);
        try {
          if (language === "javascript") engine.initScript(String(code));
          else engine.initSource(String(code), language);
        } catch (thrown) {
          throw failure(engine, thrown, "init");
        }
        return { output: drain(() => engine.takeOutput(), line => line) };
      } finally {
        closeEngine(engine);
      }
    },

    /** Create a sandbox for one program; see README.md. */
    createSandbox(sandboxOptions = {}) {
      return createSandbox(zipp, sandboxOptions);
    },

    /**
     * Add the torch package to this javascript-python-base instance (an
     * isolated sandbox loads its own copy: pass `packages` to loadZipp).
     */
    async addTorch(torchSource) {
      if (!hasPython) throw new Error(`zipp: the ${meta.variant} engine has no Python`);
      const torch = await compileZipp(torchSource);
      const torchMeta = readZippMeta(torch);
      if (torchMeta && (torchMeta.variant !== "torch-package" || torchMeta.version !== meta.version)) {
        throw new Error(`zipp: expected the ${meta.version} torch package, got ${torchMeta.variant} ${torchMeta.version}`);
      }
      const result = installTorch(bindings, await WebAssembly.instantiate(torch, {}));
      zipp.packages = Object.freeze([...zipp.packages, torch]);
      return result;
    },
  };
  return zipp;
}

// --- Sandboxes ---------------------------------------------------------------

/** A sandbox failure: `category` says what kind, `terminal` whether the sandbox is gone. */
export class ZippSandboxError extends Error {
  constructor(category, message, { terminal = false, output = [] } = {}) {
    super(message);
    this.name = "ZippSandboxError";
    this.category = category;
    this.terminal = terminal;
    this.output = output;
  }
}

function closeEngine(engine) {
  try {
    engine.dispose();
  } catch {
    // Already disposed by a terminal failure.
  }
  try {
    engine.free();
  } catch {
    // Already freed.
  }
}

// The engine's own account of a thrown error: its kind (guest, source,
// resource, conversion, usage) and whether it disposed the engine, never
// the message text a guest controls.
function failure(engine, thrown, phase) {
  const error = asError(thrown);
  let kind = "guest";
  try {
    kind = engine.lastErrorKind() || kind;
  } catch {
    // A freed engine answers nothing.
  }
  let output = [];
  try {
    output = phase === "init"
      ? drain(() => engine.takeFailedConsole(), entry => entry.text)
      : drain(() => engine.takeOutput(), line => line);
  } catch {
    // Nothing more to read.
  }
  const terminal = phase === "init" || kind === "resource" || engine.disposed === true;
  const wrapped = new ZippSandboxError(kind, error.message, { terminal, output });
  wrapped.cause = error;
  return wrapped;
}

const SANDBOX_OPTIONS = new Set(["language", "isolate", "limits", "permissions", "host", "workerUrl"]);
const LIMIT_OPTIONS = new Set(["instructions", "timeoutMs"]);
const PERMISSION_OPTIONS = new Set(["storage", "files", "network"]);
const NETWORK_OPTIONS = new Set(["allow", "methods", "maxResponseBytes", "timeoutMs", "redirects", "fetch"]);
const STORAGE_OPTIONS = new Set(["initial", "quota", "backend", "prefix"]);
const FILES_OPTIONS = new Set(["files", "writable", "quota"]);
const DEFAULT_TIMEOUT_MS = 30000;
const MAX_RESPONSE_BYTES = 1000000; // a guest string holds at most 1,048,576 bytes
const STORAGE_QUOTA = 5000000;
const FILES_QUOTA = 16000000;

function sizeLimit(value, what) {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`zipp: ${what} must be a nonnegative integer`);
  }
  return value;
}

function timeLimit(value, what) {
  if (typeof value !== "number" || !Number.isInteger(value) || value <= 0 || value > 2147483647) {
    throw new TypeError(`zipp: ${what} must be a number of milliseconds in (0, 2147483647]`);
  }
  return value;
}

function mapSize(entries) {
  let size = 0;
  for (const [key, value] of new Map(entries)) size += key.length + value.length;
  return size;
}

function onlyKeys(object, allowed, what) {
  for (const key of Object.keys(object)) {
    if (!allowed.has(key)) throw new TypeError(`zipp: unknown ${what} option "${key}"`);
  }
  // Missing options must not be supplied by a polluted Object.prototype.
  return Object.assign(Object.create(null), object);
}

function plainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// Paths in a sandbox's file map: "/" separated, relative, no "." or ".."
// segments, so "./a/b.txt" and "a/b.txt" name one file and nothing escapes.
function filePath(path) {
  const parts = String(path).replace(/\\/g, "/").split("/").filter(p => p !== "" && p !== ".");
  if (parts.length === 0 || parts.includes("..")) throw new TypeError(`zipp: invalid sandbox path "${path}"`);
  return parts.join("/");
}

// The options, checked strictly (an unknown key is an error, not a silently
// ignored permission), split into what crosses to a Worker (`spec`, plain
// data) and what stays where the sandbox was created (functions, a trusted
// storage backend).
function normalize(zipp, options) {
  if (!plainObject(options)) throw new TypeError("zipp: sandbox options must be an object");
  options = onlyKeys(options, SANDBOX_OPTIONS, "sandbox");
  const language = options.language ?? "javascript";
  if (!zipp.languages.includes(language)) {
    throw new TypeError(`zipp: the ${zipp.meta.variant} engine runs ${zipp.languages.join(" and ")}, not ${language}`);
  }
  let limits = options.limits ?? {};
  if (!plainObject(limits)) throw new TypeError("zipp: limits must be an object");
  limits = onlyKeys(limits, LIMIT_OPTIONS, "limits");
  const timeoutMs = timeLimit(limits.timeoutMs ?? DEFAULT_TIMEOUT_MS, "limits.timeoutMs");
  const instructions = limits.instructions ?? null;
  if (instructions !== null && !(typeof instructions === "number" && Number.isSafeInteger(instructions) && instructions > 0)) {
    throw new TypeError("zipp: limits.instructions must be a positive integer");
  }

  let permissions = options.permissions ?? {};
  if (!plainObject(permissions)) throw new TypeError("zipp: permissions must be an object");
  permissions = onlyKeys(permissions, PERMISSION_OPTIONS, "permissions");
  const local = { backend: null, network: null, host: new Map() };

  let storage = null;
  if (permissions.storage) {
    let s = permissions.storage === true ? {} : permissions.storage;
    if (!plainObject(s)) throw new TypeError("zipp: permissions.storage must be true or an object");
    s = onlyKeys(s, STORAGE_OPTIONS, "storage");
    const initial = s.initial ?? {};
    if (!plainObject(initial)) throw new TypeError("zipp: permissions.storage.initial must be an object of strings");
    if (s.backend && (s.quota !== undefined || Object.keys(initial).length)) {
      throw new TypeError("zipp: a storage backend owns its data and quota; omit initial and quota");
    }
    if (s.backend && s.prefix !== undefined && (typeof s.prefix !== "string" || s.prefix.length === 0)) {
      throw new TypeError("zipp: permissions.storage.prefix must be a nonempty string");
    }
    storage = {
      initial: Object.entries(initial).map(([k, v]) => [String(k), String(v)]),
      quota: sizeLimit(s.quota ?? STORAGE_QUOTA, "permissions.storage.quota"),
      prefix: s.backend ? (s.prefix ?? "zipp:") : null,
    };
    if (mapSize(storage.initial) > storage.quota) {
      throw new TypeError("zipp: permissions.storage.initial exceeds its quota");
    }
    if (s.backend) {
      for (const m of ["getItem", "setItem", "removeItem", "key"]) {
        if (typeof s.backend[m] !== "function") throw new TypeError(`zipp: permissions.storage.backend needs ${m}()`);
      }
      local.backend = s.backend;
    }
  }

  let files = null;
  if (permissions.files) {
    let f = plainObject(permissions.files) && Object.hasOwn(permissions.files, "files") ? permissions.files : { files: permissions.files };
    if (!plainObject(f.files)) throw new TypeError("zipp: permissions.files must map paths to text");
    f = onlyKeys(f, FILES_OPTIONS, "files");
    files = {
      entries: Object.entries(f.files).map(([p, text]) => [filePath(p), String(text)]),
      writable: f.writable === true,
      quota: sizeLimit(f.quota ?? FILES_QUOTA, "permissions.files.quota"),
    };
    if (mapSize(files.entries) > files.quota) {
      throw new TypeError("zipp: permissions.files entries exceed their quota");
    }
  }

  let network = false;
  if (permissions.network) {
    let n = permissions.network;
    if (n === true) n = { allow: ["*"] };
    else if (Array.isArray(n) || typeof n === "string") n = { allow: [].concat(n) };
    if (!plainObject(n)) throw new TypeError("zipp: permissions.network must be true, a list of allowed URLs, or an object");
    n = onlyKeys(n, NETWORK_OPTIONS, "network");
    const allow = [].concat(n.allow ?? []).map(String);
    if (allow.length === 0) throw new TypeError("zipp: permissions.network.allow lists no URL");
    for (const entry of allow) {
      if (entry === "*") continue;
      let url;
      try { url = new URL(entry); } catch { /* rejected below */ }
      if (!/^https?:\/\//i.test(entry) || !url || !["http:", "https:"].includes(url.protocol)) {
        throw new TypeError(`zipp: allowed network entry "${entry}" is not an http(s) URL or "*"`);
      }
      if (url.username || url.password) {
        throw new TypeError(`zipp: allowed network entry "${entry}" must not contain credentials`);
      }
    }
    local.network = {
      allow,
      methods: [].concat(n.methods ?? ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"]).map(m => String(m).toUpperCase()),
      maxResponseBytes: sizeLimit(n.maxResponseBytes ?? MAX_RESPONSE_BYTES, "permissions.network.maxResponseBytes"),
      timeoutMs: timeLimit(n.timeoutMs ?? DEFAULT_TIMEOUT_MS, "permissions.network.timeoutMs"),
      redirects: n.redirects === true,
      fetch: n.fetch ?? globalThis.fetch?.bind(globalThis),
    };
    if (typeof local.network.fetch !== "function") throw new TypeError("zipp: this environment has no fetch; pass permissions.network.fetch");
    network = true;
  }

  const host = options.host ?? {};
  if (!plainObject(host)) throw new TypeError("zipp: host must map host.call kinds to functions");
  for (const [kind, fn] of Object.entries(host)) {
    if (typeof fn !== "function") throw new TypeError(`zipp: host["${kind}"] is not a function`);
    if (kind === "net.fetch") throw new TypeError(`zipp: "net.fetch" is the network permission's; use permissions.network`);
    local.host.set(kind, fn);
  }

  if (language !== "javascript" && (storage || network || local.host.size)) {
    throw new TypeError("zipp: a Python program reaches no storage, network or host functions; it may only be given files (as its importable modules)");
  }
  if (language !== "javascript" && files && (files.writable || files.entries.some(([p]) => !/^[A-Za-z_][A-Za-z0-9_]*\.py$/.test(p)))) {
    throw new TypeError("zipp: a Python program's files are its modules: read-only `name.py` entries");
  }

  const isolate = options.isolate ?? true;
  if (typeof isolate !== "boolean") throw new TypeError("zipp: isolate must be a boolean");
  if (isolate && local.backend) {
    throw new TypeError("zipp: a storage backend stays on this thread; give an isolated sandbox `initial` data instead, or set isolate: false");
  }
  const spec = { language, instructions, timeoutMs, storage, files, network };
  return { spec, local, isolate, workerUrl: options.workerUrl ?? null };
}

// --- The engine side: one program, its bridges and its host-call pump ------

// Appended to a program granted the network: a hoisted declaration, so it
// is callable from the first line and moves no line number. Without the
// grant there is no `fetch` at all.
const FETCH_HELPER = `
;function fetch(input, init) {
  init = init || {};
  var url = String(input !== null && typeof input === "object" && input.url !== undefined ? input.url : input);
  var headers = {};
  var given = init.headers || {};
  if (Array.isArray(given)) { for (var i = 0; i < given.length; i++) headers[String(given[i][0]).toLowerCase()] = String(given[i][1]); }
  else if (typeof given.forEach === "function") { given.forEach(function (v, k) { headers[String(k).toLowerCase()] = String(v); }); }
  else { for (var k in given) headers[String(k).toLowerCase()] = String(given[k]); }
  var request = { method: String(init.method || "GET").toUpperCase(), headers: headers, body: init.body == null ? null : String(init.body) };
  return new Promise(function (resolve, reject) {
    host.call("net.fetch", [url, JSON.stringify(request)], function (reply) {
      if (!reply || !reply.ok) { reject(new TypeError("fetch: " + (reply && reply.error ? reply.error.message : "no response"))); return; }
      var v = reply.value, h = {};
      for (var j = 0; j < v.headers.length; j++) h[v.headers[j][0]] = v.headers[j][1];
      resolve({
        ok: v.status >= 200 && v.status < 300, status: v.status, statusText: v.statusText, url: v.url,
        headers: {
          get: function (name) { var x = h[String(name).toLowerCase()]; return x === undefined ? null : x; },
          has: function (name) { return h[String(name).toLowerCase()] !== undefined; },
          forEach: function (fn) { for (var n in h) fn(h[n], n); },
        },
        text: function () { return Promise.resolve(v.body); },
        json: function () { return Promise.resolve(v.body).then(JSON.parse); },
      });
    });
  });
}
`;

function frozenBridge(methods) {
  const bridge = Object.create(null);
  for (const [name, fn] of Object.entries(methods)) {
    Object.defineProperty(bridge, name, { value: fn, enumerable: true });
  }
  return Object.freeze(bridge);
}

// localStorage for the guest: a private in-memory store, or a host-given
// backend seen only through a key prefix.
function makeStorage(spec, backend) {
  const memory = new Map(spec.initial);
  let used = 0;
  for (const [k, v] of memory) used += k.length + v.length;
  const prefix = spec.prefix;
  const ownKeys = () => {
    const keys = [];
    for (let i = 0; i < backend.length; i++) {
      const k = backend.key(i);
      if (k !== null && k.startsWith(prefix)) keys.push(k);
    }
    return keys;
  };
  const bridge = backend
    ? frozenBridge({
        getItem: k => backend.getItem(prefix + k),
        setItem: (k, v) => backend.setItem(prefix + k, v),
        removeItem: k => backend.removeItem(prefix + k),
        clear: () => { for (const k of ownKeys()) backend.removeItem(k); },
      })
    : frozenBridge({
        getItem: k => (memory.has(k) ? memory.get(k) : null),
        setItem: (k, v) => {
          const before = memory.has(k) ? k.length + memory.get(k).length : 0;
          if (used - before + k.length + v.length > spec.quota) throw new Error("storage quota exceeded");
          used += k.length + v.length - before;
          memory.set(k, v);
        },
        removeItem: k => {
          if (memory.has(k)) used -= k.length + memory.get(k).length;
          memory.delete(k);
        },
        clear: () => { memory.clear(); used = 0; },
      });
  return { bridge, snapshot: () => (backend ? null : Object.fromEntries(memory)) };
}

// The guest's files: host.callSync("fs.read", path) and kin, over a map.
function makeFiles(spec) {
  const files = new Map(spec.entries);
  let used = 0;
  for (const [p, t] of files) used += p.length + t.length;
  const bridge = frozenBridge({
    call(kind, args) {
      switch (kind) {
        case "fs.read": {
          const p = filePath(args[0]);
          return files.has(p) ? files.get(p) : null;
        }
        case "fs.exists":
          return files.has(filePath(args[0]));
        case "fs.list": {
          const dir = args.length && args[0] !== "" ? `${filePath(args[0])}/` : "";
          return [...files.keys()].filter(p => p.startsWith(dir)).sort();
        }
        case "fs.write": {
          const p = filePath(args[0]);
          const text = String(args[1] ?? "");
          const before = files.has(p) ? p.length + files.get(p).length : 0;
          if (used - before + p.length + text.length > spec.quota) return { error: "file quota exceeded" };
          used += p.length + text.length - before;
          files.set(p, text);
          return true;
        }
        case "fs.remove": {
          const p = filePath(args[0]);
          if (files.has(p)) used -= p.length + files.get(p).length;
          return files.delete(p);
        }
        default:
          return { error: `unknown operation ${kind}` };
      }
    },
  });
  return { bridge, snapshot: () => Object.fromEntries(files) };
}

// One program in one engine. `dispatch(kind, args, signal)` answers its
// host.call requests (a promise of the engine's reply shape); it runs here,
// or on the thread that created an isolated sandbox. `signal` aborts when
// the request is given up: its time limit passed, or the sandbox closed.
function sandboxCore(bindings, spec, dispatch, backend) {
  const engine = new bindings.Engine();
  const storage = spec.storage ? makeStorage(spec.storage, backend) : null;
  const files = spec.files ? makeFiles(spec.files) : null;
  let state = "new"; // new -> ready -> dead
  let dead = null;
  const output = [];
  // The requests runs and calls are waiting on, and how `close()` ends every
  // such wait at once (rather than at its time limit).
  const outstanding = new Set();
  const wakers = new Set();

  if (spec.language === "javascript") {
    const grants = [];
    if (storage) {
      engine.setLocalStorageBridge(storage.bridge);
      grants.push("ls.getItem", "ls.setItem", "ls.removeItem", "ls.clear");
    }
    if (files) {
      engine.setAppBridge(files.bridge);
      grants.push("app.fs.read", "app.fs.exists", "app.fs.list");
      if (spec.files.writable) grants.push("app.fs.write", "app.fs.remove");
    }
    engine.setSyncHostCapabilities(grants);
  }
  if (spec.instructions !== null) engine.setInstructionBudget(spec.instructions);

  function check() {
    if (dead) throw new ZippSandboxError(dead.category, `zipp: the sandbox has stopped (${dead.message})`, { terminal: true });
  }
  function kill(error) {
    dead = error instanceof ZippSandboxError ? error : new ZippSandboxError("host", String(error?.message ?? error), { terminal: true });
    dead.terminal = true;
    closeEngine(engine);
    state = "dead";
    return dead;
  }
  function collect() {
    for (;;) {
      let page;
      try {
        page = engine.takeOutput();
      } catch (error) {
        if (engine.disposed === true) return;
        throw error;
      }
      if (!Array.isArray(page) || page.length === 0) return;
      for (const line of page) output.push(line);
    }
  }

  // Deliver every host.call the program has made, and every one its
  // callbacks make, until none is outstanding or the time runs out.
  async function settle(deadline) {
    const inflight = new Map();
    let wake;
    const closing = new Promise(resolve => { wake = resolve; });
    wakers.add(wake);
    try {
      return await settleUntil(deadline, inflight, closing);
    } finally {
      wakers.delete(wake);
    }
  }
  async function settleUntil(deadline, inflight, closing) {
    const abandon = reason => {
      for (const { controller } of inflight.values()) {
        controller.abort(reason);
        outstanding.delete(controller);
      }
    };
    for (;;) {
      check();
      let status;
      do {
        status = engine.drainPendingHostCallsStatus();
        for (const call of status.calls) {
          const controller = new AbortController();
          outstanding.add(controller);
          const reply = Promise.resolve()
            .then(() => dispatch(call.kind, call.args, controller.signal))
            .then(
              value => value,
              error => ({ ok: false, error: { code: "host", message: describeError(error) } }),
            )
            .then(result => ({ call, result }));
          inflight.set(call.id, { reply, controller });
        }
      } while (status.hasMore);
      if (inflight.size === 0) return;
      const remaining = deadline - Date.now();
      let timer;
      const timeout = new Promise(resolve => { timer = setTimeout(() => resolve(null), Math.max(0, remaining)); });
      const next = await Promise.race([...[...inflight.values()].map(entry => entry.reply), timeout, closing]);
      clearTimeout(timer);
      if (next === "closed") {
        abandon(new ZippSandboxError("closed", "zipp: the sandbox is closed", { terminal: true }));
        throw new ZippSandboxError("closed", "zipp: the sandbox was closed with host request(s) pending", { terminal: true });
      }
      if (next === null) {
        abandon(new ZippSandboxError("timeout", "zipp: the time limit passed"));
        for (const id of inflight.keys()) {
          try { engine.cancelHostCallback(id); } catch { /* disposed */ }
        }
        collect();
        throw new ZippSandboxError("timeout", `zipp: ${inflight.size} host request(s) still pending after the time limit`, { output: output.splice(0) });
      }
      outstanding.delete(inflight.get(next.call.id).controller);
      inflight.delete(next.call.id);
      try {
        engine.resolveHostCallback(next.call.id, next.result);
      } catch (thrown) {
        const error = failure(engine, thrown, "call");
        if (error.terminal) throw kill(error);
        throw error;
      }
      collect();
    }
  }

  return {
    async run(code) {
      check();
      if (state !== "new") throw new ZippSandboxError("usage", "zipp: a sandbox runs one program; call its functions, or create another sandbox");
      state = "ready";
      const deadline = Date.now() + spec.timeoutMs;
      let symbols = null;
      try {
        if (spec.language === "javascript") {
          let source = String(code);
          // A program that declares its own `fetch` keeps it.
          if (spec.network && !/\b(?:let|const|class|var|function)\s+fetch\b/.test(source)) source += FETCH_HELPER;
          symbols = engine.initScript(source);
        } else if (spec.files && spec.files.entries.length) {
          const modules = { __main__: String(code) };
          for (const [p, text] of spec.files.entries) modules[p.slice(0, -3)] = text;
          engine.initPythonProject(modules, "__main__", []);
        } else {
          engine.initSource(String(code), spec.language);
        }
      } catch (thrown) {
        throw kill(failure(engine, thrown, "init"));
      }
      collect();
      await settle(deadline);
      return { output: output.splice(0), symbols };
    },
    async call(name, args = []) {
      check();
      if (state !== "ready") throw new ZippSandboxError("usage", "zipp: run the program before calling its functions");
      const deadline = Date.now() + spec.timeoutMs;
      let value;
      try {
        value = spec.language === "javascript" ? engine.callFunction(String(name), args) : engine.pythonCall(String(name), args);
      } catch (thrown) {
        const error = failure(engine, thrown, "call");
        if (error.terminal) throw kill(error);
        throw error;
      }
      collect();
      await settle(deadline);
      return { value, output: output.splice(0) };
    },
    snapshot() {
      return { storage: storage ? storage.snapshot() : null, files: files ? files.snapshot() : null };
    },
    usage() {
      check();
      return engine.resourceUsage();
    },
    close() {
      if (state !== "dead") {
        closeEngine(engine);
        state = "dead";
        dead = new ZippSandboxError("closed", "closed", { terminal: true });
      }
      for (const controller of outstanding) controller.abort(dead);
      outstanding.clear();
      for (const wake of wakers) wake("closed");
    },
  };
}

// --- The creating side: permissions it serves, and isolation ---------------

function allowed(url, allow) {
  return allow.some(entry => {
    if (entry === "*") return true;
    const base = new URL(entry);
    // A canonical bare origin grants its whole origin. Other entries are URL
    // prefixes; a root slash prefix also covers every path on that origin.
    return entry === base.origin ? url.origin === base.origin : url.href.startsWith(base.href);
  });
}

async function readLimited(response, max) {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.length;
    if (size > max) {
      await reader.cancel();
      throw Object.assign(new Error(`response is larger than ${max} bytes`), { code: "too-large" });
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(size);
  let at = 0;
  for (const c of chunks) {
    bytes.set(c, at);
    at += c.length;
  }
  return new TextDecoder().decode(bytes);
}

const FORBIDDEN_HEADERS = new Set([
  "accept-charset", "accept-encoding", "access-control-request-headers", "access-control-request-method",
  "connection", "content-length", "cookie", "cookie2", "date", "dnt", "expect", "host", "keep-alive",
  "origin", "referer", "set-cookie", "te", "trailer", "transfer-encoding", "upgrade", "via",
]);

const deny = (code, message) => ({ ok: false, error: { code, message } });

// With `redirects`, at most this many are followed, each authorized before
// it is requested.
const MAX_REDIRECTS = 5;
const REDIRECT_STATUSES = new Set([301, 302, 303, 307, 308]);
// Headers that describe a request body, dropped with the body when a
// redirect turns the request into a GET (the Fetch standard's list).
const BODY_HEADERS = ["content-type", "content-encoding", "content-language", "content-location"];

// The guest's `fetch` (and host.call("net.fetch")): an absolute http(s) URL
// the allowlist names, a method it allows, no cookies or credentials, a
// redirect only when permitted and to an allowed URL, a time limit and a
// size limit on the response.
async function serveFetch(policy, args, cancel) {
  let url;
  let request;
  try {
    url = new URL(String(args[0]));
    request = JSON.parse(String(args[1] ?? "{}"));
  } catch {
    return deny("bad-request", "fetch needs an absolute URL");
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return deny("denied", `${url.protocol} URLs are not permitted`);
  if (!allowed(url, policy.allow)) return deny("denied", `network access to ${url.origin} is not permitted`);
  let method = String(request.method || "GET").toUpperCase();
  if (!policy.methods.includes(method)) return deny("denied", `${method} requests are not permitted`);
  // The Fetch standard's forbidden request headers, dropped everywhere (a
  // browser drops them itself; Node's fetch would send them): the guest
  // cannot send cookies, pose as another host or origin, or shape the
  // connection.
  const headers = {};
  if (plainObject(request.headers)) {
    for (const [name, value] of Object.entries(request.headers)) {
      const n = String(name).toLowerCase();
      if (FORBIDDEN_HEADERS.has(n) || n.startsWith("proxy-") || n.startsWith("sec-")) continue;
      headers[n] = String(value);
    }
  }
  // One time limit for the request and all its redirects, and the request's
  // own signal (the sandbox closed, or gave the request up).
  const timeout = AbortSignal.timeout(policy.timeoutMs);
  const signal = cancel ? eitherSignal(timeout, cancel) : timeout;
  let body = request.body ?? undefined;
  try {
    for (let hops = 0; ; hops++) {
      const response = await policy.fetch(url.href, {
        method,
        headers,
        body,
        credentials: "omit",
        // fetch never follows a redirect itself: a permitted one is checked
        // below before it is requested, so a URL the allowlist does not name
        // is never contacted ("follow" would request it, then check).
        redirect: policy.redirects ? "manual" : "error",
        signal,
      });
      if (policy.redirects && response.type === "opaqueredirect") {
        // A browser's fetch hides a manual redirect's destination, so it
        // cannot be checked: the redirect fails the request.
        return deny("denied", "the request was redirected, and this environment's fetch does not say where, so the redirect cannot be checked");
      }
      if (policy.redirects && REDIRECT_STATUSES.has(response.status) && response.headers.has("location")) {
        try { await response.body?.cancel(); } catch {}
        if (hops === MAX_REDIRECTS) return deny("network", `more than ${MAX_REDIRECTS} redirects`);
        let next;
        try {
          next = new URL(response.headers.get("location"), url);
        } catch {
          return deny("network", "the request was redirected to an invalid URL");
        }
        if ((next.protocol !== "https:" && next.protocol !== "http:") || !allowed(next, policy.allow)) {
          return deny("denied", `the request was redirected to ${next.origin}, which is not permitted`);
        }
        // The Fetch standard's rules: a 303 (except after HEAD), or a 301 or
        // 302 after a POST, goes on as a GET without its body.
        if ((response.status === 303 && method !== "HEAD") || ((response.status === 301 || response.status === 302) && method === "POST")) {
          method = "GET";
          body = undefined;
          for (const name of BODY_HEADERS) delete headers[name];
          if (!policy.methods.includes(method)) return deny("denied", `the request was redirected as a ${method} request, which is not permitted`);
        }
        // Credentials stay with the origin they were given for.
        if (next.origin !== url.origin) delete headers.authorization;
        url = next;
        continue;
      }
      if (response.url && !allowed(new URL(response.url), policy.allow)) {
        return deny("denied", `the request was redirected to ${new URL(response.url).origin}, which is not permitted`);
      }
      const text = await readLimited(response, policy.maxResponseBytes);
      return {
        ok: true,
        value: {
          status: response.status,
          statusText: response.statusText,
          url: response.url || url.href,
          headers: [...response.headers].map(([k, v]) => [k.toLowerCase(), v]),
          body: text,
        },
      };
    }
  } catch (error) {
    if (cancel?.aborted) return deny("cancelled", "the request was cancelled");
    return deny(error?.code === "too-large" ? "too-large" : error?.name === "TimeoutError" ? "timeout" : "network", describeError(error));
  }
}

// At most this many of one sandbox's host requests run at once; the rest
// wait, in order, without starting.
const MAX_RUNNING_HOST_REQUESTS = 64;
// The longest error message a host function's failure passes to the guest.
const MAX_HOST_ERROR_CHARS = 1000;

// An error as bounded text, whatever it is: its getters may throw, and its
// message may be any length.
function describeError(error) {
  let text;
  try { text = String(error?.message ?? error); } catch { text = "<unprintable error>"; }
  return text.length > MAX_HOST_ERROR_CHARS ? text.slice(0, MAX_HOST_ERROR_CHARS) + "…" : text;
}

// A signal that aborts when any of `signals` does.
function eitherSignal(...signals) {
  if (typeof AbortSignal.any === "function") return AbortSignal.any(signals);
  const controller = new AbortController();
  for (const s of signals) {
    if (s.aborted) { controller.abort(s.reason); break; }
    s.addEventListener("abort", () => controller.abort(s.reason), { once: true });
  }
  return controller.signal;
}

// The host requests of one sandbox (the guest's host.call and fetch), which
// the sandbox owns. `dispatch(kind, args, signal)` answers one; `signal` is
// the requester's: it aborts when the request is abandoned (its time limit
// passed, or the isolated sandbox's Worker gave it up). Each request runs
// with a signal that also aborts when the sandbox closes: `this.signal` in a
// host function, the request's own in fetch. A request that ignores it keeps
// its running slot until it actually finishes, so abandoned work cannot be
// replaced by new work past the limit; what it finally answers is discarded,
// as the requester has gone. After `close()` nothing starts.
function dispatcher(local) {
  const owner = new AbortController();
  const waiting = [];
  let running = 0;
  function serve(kind, args, signal) {
    if (kind === "net.fetch") {
      return local.network ? serveFetch(local.network, args, signal) : deny("denied", "network access is not permitted");
    }
    const fn = local.host.get(kind);
    if (!fn) return deny("denied", `host.call("${kind}") is not permitted`);
    return Promise.resolve()
      .then(() => fn.apply({ signal, kind }, args))
      .then(value => ({ ok: true, value }), error => deny("host", describeError(error)));
  }
  function start(item) {
    running++;
    const signal = eitherSignal(item.signal, owner.signal);
    Promise.resolve()
      .then(() => serve(item.kind, item.args, signal))
      .then(result => result, error => deny("host", describeError(error)))
      .then(result => {
        running--;
        item.resolve(result);
        pump();
      });
  }
  function pump() {
    while (!owner.signal.aborted && running < MAX_RUNNING_HOST_REQUESTS && waiting.length) start(waiting.shift());
  }
  const cancelled = (why) => deny("cancelled", `the request was ${why} before it started`);
  return {
    dispatch(kind, args, signal) {
      if (owner.signal.aborted) return Promise.resolve(deny("closed", "the sandbox is closed"));
      if (signal.aborted) return Promise.resolve(cancelled("abandoned"));
      return new Promise(resolve => {
        const item = { kind, args, signal, resolve };
        waiting.push(item);
        signal.addEventListener("abort", () => {
          const at = waiting.indexOf(item);
          if (at >= 0) {
            waiting.splice(at, 1);
            resolve(cancelled("abandoned"));
          }
        }, { once: true });
        pump();
      });
    },
    close(reason) {
      if (owner.signal.aborted) return;
      owner.abort(reason);
      for (const item of waiting.splice(0)) item.resolve(cancelled("closed"));
    },
    counts: () => ({ hostRequestsRunning: running, hostRequestsWaiting: waiting.length }),
  };
}

function sandboxApi(impl) {
  return Object.freeze({
    /** Run the program once; resolves to `{ output, symbols }` when its host calls are done. */
    run: code => impl.request("run", [code]),
    /** Call one of its top-level functions; resolves to `{ value, output }`. */
    call: (name, args) => impl.request("call", [name, args]),
    /** Its storage and files as they are now: `{ storage, files }`. */
    snapshot: () => impl.request("snapshot", []),
    /** The engine's resource account (heap, steps, output). */
    usage: () => impl.request("usage", []),
    /** End it; an isolated sandbox's Worker ends with it. */
    close: () => impl.close(),
    get closed() { return impl.closed(); },
  });
}

async function createSandbox(zipp, options) {
  const { spec, local, isolate, workerUrl } = normalize(zipp, options);
  const requests = dispatcher(local);
  if (!isolate) {
    const core = sandboxCore(zipp.bindings, spec, requests.dispatch, local.backend);
    let closed = false;
    return sandboxApi({
      request: async (op, args) => {
        if (closed) throw new ZippSandboxError("closed", "zipp: the sandbox is closed", { terminal: true });
        const value = await core[op](...args);
        return op === "usage" ? { ...value, ...requests.counts() } : value;
      },
      close: async () => {
        closed = true;
        core.close();
        requests.close(new ZippSandboxError("closed", "zipp: the sandbox is closed", { terminal: true }));
      },
      closed: () => closed,
    });
  }
  return isolatedSandbox(zipp, spec, requests, workerUrl);
}

// A Worker (browsers, Deno, Bun) or worker thread (Node) running this same
// file, with the engine and packages compiled once and posted to it. A
// request that outlives `limits.timeoutMs` terminates it: the only way to
// stop a program that never yields.
async function isolatedSandbox(zipp, spec, requests, workerUrl) {
  const url = workerUrl ? new URL(workerUrl, globalThis.location?.href) : new URL(import.meta.url);
  let port;
  if (nodeLike() && typeof globalThis.Worker !== "function") {
    const { Worker } = await import("node:worker_threads");
    const worker = new Worker(url, { workerData: { zippSandbox: true } });
    port = {
      post: message => worker.postMessage(message),
      listen: fn => {
        worker.on("message", fn);
        worker.on("error", error => fn({ type: "crash", message: String(error?.message ?? error) }));
      },
      terminate: () => worker.terminate(),
    };
  } else if (typeof Worker === "function") {
    const worker = new Worker(url, { type: "module", name: WORKER_NAME });
    port = {
      post: message => worker.postMessage(message),
      listen: fn => {
        worker.addEventListener("message", event => fn(event.data));
        worker.addEventListener("error", event => fn({ type: "crash", message: String(event.message || "worker error") }));
      },
      terminate: () => worker.terminate(),
    };
  } else {
    throw new Error("zipp: this environment has no Worker; create the sandbox with isolate: false");
  }

  const pending = new Map();
  // The Worker's host requests being served here, by its id for them, so the
  // Worker can give one up (`host-abandon`).
  const serving = new Map();
  let nextId = 0;
  let dead = null;
  function die(error) {
    if (dead) return;
    dead = error;
    port.terminate();
    requests.close(error);
    for (const p of pending.values()) {
      clearTimeout(p.timer);
      p.reject(error);
    }
    pending.clear();
  }
  port.listen(async message => {
    if (dead) return;
    if (message.type === "host") {
      const controller = new AbortController();
      serving.set(message.hid, controller);
      const result = await requests.dispatch(message.kind, message.args, controller.signal);
      serving.delete(message.hid);
      if (!dead && !controller.signal.aborted) {
        try {
          port.post({ type: "host-reply", hid: message.hid, result });
        } catch {
          // A granted host function may return a value that cannot cross the
          // Worker boundary. Answer the call instead of rejecting this listener.
          try {
            port.post({ type: "host-reply", hid: message.hid, result: deny("host", "the host result cannot be sent to the sandbox") });
          } catch {
            die(new ZippSandboxError("host", "zipp: the sandbox Worker could not receive a host reply", { terminal: true }));
          }
        }
      }
    } else if (message.type === "host-abandon") {
      serving.get(message.hid)?.abort(new ZippSandboxError("timeout", "zipp: the sandbox gave the request up"));
    } else if (message.type === "crash") {
      die(new ZippSandboxError("host", `zipp: the sandbox Worker failed: ${message.message}`, { terminal: true }));
    } else if (message.id !== undefined && pending.has(message.id)) {
      const p = pending.get(message.id);
      pending.delete(message.id);
      clearTimeout(p.timer);
      if (message.ok) p.resolve(message.value);
      else {
        const e = message.error;
        const error = new ZippSandboxError(e.category, e.message, { terminal: e.terminal, output: e.output });
        if (e.terminal) die(error);
        p.reject(error);
      }
    }
  });
  function request(op, args, timeoutMs = spec.timeoutMs) {
    if (dead) return Promise.reject(new ZippSandboxError(dead.category, `zipp: the sandbox has stopped (${dead.message})`, { terminal: true }));
    const id = ++nextId;
    return new Promise((resolve, reject) => {
      // A little longer than the Worker's own host-call limit, so a pending
      // host call reports as such before the Worker is ended.
      const timer = setTimeout(() => {
        die(new ZippSandboxError("timeout", `zipp: the program did not finish within ${spec.timeoutMs} ms; its Worker was terminated`, { terminal: true }));
      }, timeoutMs + 250);
      pending.set(id, { resolve, reject, timer });
      try {
        port.post({ type: op, id, args });
      } catch (error) {
        pending.delete(id);
        clearTimeout(timer);
        reject(new ZippSandboxError("usage", `zipp: ${op} arguments cannot be sent to the Worker: ${error.message}`));
      }
    });
  }
  // Loading compiles nothing again but does instantiate: give it its own
  // allowance.
  await request("init", [{ engine: zipp.module, packages: [...zipp.packages], spec }], Math.max(spec.timeoutMs, 30000));
  return sandboxApi({
    request: (op, args) => op === "usage" ? request(op, args).then(usage => ({ ...usage, ...requests.counts() })) : request(op, args),
    close: async () => { die(new ZippSandboxError("closed", "zipp: the sandbox is closed", { terminal: true })); },
    closed: () => dead !== null,
  });
}

// --- This file as the sandbox Worker -----------------------------------------

function serveSandbox(port) {
  let core = null;
  let hostId = 0;
  const hostWaits = new Map();
  // Served by the creating side; a request given up here is given up there.
  const dispatch = (kind, args, signal) =>
    new Promise(resolve => {
      const hid = ++hostId;
      hostWaits.set(hid, resolve);
      signal.addEventListener("abort", () => {
        if (!hostWaits.delete(hid)) return;
        port.post({ type: "host-abandon", hid });
        resolve({ ok: false, error: { code: "cancelled", message: "the request was given up" } });
      }, { once: true });
      port.post({ type: "host", hid, kind, args });
    });
  port.listen(async message => {
    if (message.type === "host-reply") {
      const resolve = hostWaits.get(message.hid);
      hostWaits.delete(message.hid);
      resolve?.(message.result);
      return;
    }
    const reply = (ok, value) => port.post({ type: "result", id: message.id, ok, ...(ok ? { value } : { error: value }) });
    try {
      if (message.type === "init") {
        const [{ engine, packages, spec }] = message.args;
        const { bindings } = await instantiate(engine, packages);
        core = sandboxCore(bindings, spec, dispatch, null);
        reply(true, null);
      } else if (core && typeof core[message.type] === "function" && message.type !== "close") {
        reply(true, await core[message.type](...message.args));
      } else {
        reply(false, { category: "usage", message: `zipp: unknown sandbox request ${message.type}`, terminal: false, output: [] });
      }
    } catch (error) {
      reply(false, {
        category: error.category ?? "host",
        message: String(error.message ?? error),
        terminal: error.terminal ?? true,
        output: error.output ?? [],
      });
    }
  });
}

// A Worker created by isolatedSandbox has this name; a page never runs as
// one (it has a document), whatever its window.name.
if (globalThis.name === WORKER_NAME && typeof document === "undefined" && typeof globalThis.postMessage === "function") {
  serveSandbox({
    post: message => globalThis.postMessage(message),
    listen: fn => globalThis.addEventListener("message", event => fn(event.data)),
  });
} else if (nodeLike()) {
  import("node:worker_threads").then(({ isMainThread, workerData, parentPort }) => {
    if (!isMainThread && workerData?.zippSandbox === true) {
      serveSandbox({ post: message => parentPort.postMessage(message), listen: fn => parentPort.on("message", fn) });
    }
  }, () => {});
}
