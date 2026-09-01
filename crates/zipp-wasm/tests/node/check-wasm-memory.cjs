// Fail closed if a build or post-processing step drops the sandbox's linear-
// memory maximum. WebAssembly's JS reflection API does not expose memory limits,
// so inspect the tiny standard memory section directly.
const fs = require("node:fs");

const path = process.argv[2];
if (!path) throw new Error("usage: node check-wasm-memory.cjs <module.wasm>");
const bytes = fs.readFileSync(path);
const expectedMaxPages = 1073741824 / 65536;

function uleb(state) {
  let value = 0;
  let shift = 0;
  for (let n = 0; n < 10; n++) {
    if (state.at >= state.end) throw new Error("truncated WebAssembly LEB128");
    const byte = bytes[state.at++];
    value += (byte & 0x7f) * 2 ** shift;
    if ((byte & 0x80) === 0) return value;
    shift += 7;
  }
  throw new Error("oversized WebAssembly LEB128");
}

if (bytes.length < 8 || bytes.subarray(0, 8).toString("hex") !== "0061736d01000000") {
  throw new Error("not a WebAssembly 1.0 module");
}

const moduleImports = WebAssembly.Module.imports(new WebAssembly.Module(bytes));
const importedMemories = moduleImports.filter((entry) => entry.kind === "memory");
if (importedMemories.length !== 0) {
  throw new Error(`sandbox unexpectedly imports ${importedMemories.length} memory object(s)`);
}

// Imports are the complete ambient authority of a core WebAssembly module.
// Keep the wasm-bindgen shims on an audited semantic allowlist so adding a
// dependency cannot silently introduce fetch, WebSocket, storage, WASI, or a
// second module namespace. The hexadecimal suffix is an implementation hash
// which may change with Rust/wasm-bindgen; the sorted multiset still fixes both
// the operation names and overload counts (notably get/set/call/new).
const expectedImportStems = [
  "__wbg___wbindgen_boolean_get",
  "__wbg___wbindgen_is_function",
  "__wbg___wbindgen_is_null",
  "__wbg___wbindgen_is_object",
  "__wbg___wbindgen_is_string",
  "__wbg___wbindgen_is_undefined",
  "__wbg___wbindgen_number_get",
  "__wbg___wbindgen_string_get",
  "__wbg___wbindgen_throw",
  "__wbg_add",
  "__wbg_call",
  "__wbg_call",
  "__wbg_call",
  "__wbg_defineProperty",
  "__wbg_delete",
  "__wbg_error",
  "__wbg_get",
  "__wbg_get",
  "__wbg_has",
  "__wbg_isArray",
  "__wbg_keys",
  "__wbg_length",
  "__wbg_new",
  "__wbg_new",
  "__wbg_new",
  "__wbg_new_typed",
  "__wbg_new_with_length",
  "__wbg_now",
  "__wbg_parse",
  "__wbg_push",
  "__wbg_set",
  "__wbg_set",
  "__wbg_stack",
  "__wbg_static_accessor_GLOBAL",
  "__wbg_static_accessor_GLOBAL_THIS",
  "__wbg_static_accessor_SELF",
  "__wbg_static_accessor_WINDOW",
  "__wbg_stringify",
  "__wbindgen_cast",
  "__wbindgen_cast",
  "__wbindgen_init_externref_table",
].sort();

for (const entry of moduleImports) {
  if (entry.module !== "./zipp_wasm_bg.js" || entry.kind !== "function") {
    throw new Error(
      `unexpected WebAssembly import ${entry.module}.${entry.name}:${entry.kind}`,
    );
  }
}
const actualImportStems = moduleImports
  .map((entry) => entry.name.replace(/_[0-9a-f]{16}$/, ""))
  .sort();
if (JSON.stringify(actualImportStems) !== JSON.stringify(expectedImportStems)) {
  throw new Error(
    `WebAssembly host import surface changed:\n${actualImportStems.join("\n")}`,
  );
}
console.log(`ok audited host import surface: ${moduleImports.length} functions`);

let memories = [];
let at = 8;
while (at < bytes.length) {
  const id = bytes[at++];
  const sizeState = { at, end: bytes.length };
  const size = uleb(sizeState);
  at = sizeState.at;
  const end = at + size;
  if (end > bytes.length) throw new Error("truncated WebAssembly section");
  if (id === 5) {
    const state = { at, end };
    const count = uleb(state);
    for (let i = 0; i < count; i++) {
      const flags = uleb(state);
      const min = uleb(state);
      const max = (flags & 1) ? uleb(state) : null;
      memories.push({ flags, min, max });
    }
    if (state.at !== end) throw new Error("unexpected WebAssembly memory-section data");
  }
  at = end;
}

if (memories.length !== 1) {
  throw new Error(`expected one defined memory, found ${memories.length}`);
}
const memory = memories[0];
if ((memory.flags & 1) === 0 || memory.max !== expectedMaxPages) {
  throw new Error(
    `linear memory maximum is ${memory.max ?? "absent"} pages; expected ${expectedMaxPages}`,
  );
}
if ((memory.flags & 2) !== 0) throw new Error("sandbox memory unexpectedly became shared");
if ((memory.flags & 4) !== 0) throw new Error("sandbox memory unexpectedly became memory64");

console.log(`ok linear memory maximum: ${memory.max} pages (${memory.max * 65536} bytes)`);
