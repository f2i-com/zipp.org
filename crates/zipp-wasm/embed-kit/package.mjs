#!/usr/bin/env node
// Release packaging for the WebAssembly engines: the same steps locally and
// in CI (.github/workflows/release-build.yml). No dependencies beyond Node.
//
//   node package.mjs embed <variant> <glue.js> <in.wasm> <out-dir>
//       Add the bindings (`zipp.glue`) and a `zipp.meta` record to an engine
//       module, written as <out-dir>/<its release name>, which is printed.
//       The record's version, commit and languages are read from the engine
//       itself, instantiated with that glue; its profile is also written
//       beside the glue as PROFILE.json.
//   node package.mjs embed-torch <version> <commit> <in.wasm> <out-dir>
//       The same for the torch package module (a record, no bindings).
//   node package.mjs strip <in.wasm> <out.wasm>
//       Remove the zipp.* sections: the result is the module as built.
//   node package.mjs extract <in.wasm> <out-dir>
//       Write `zipp_wasm.js` and `zipp_wasm_bg.wasm` (stripped) and
//       `meta.json`: the glue/module pair wasm-bindgen produced, for hosts
//       that load them separately (the site's playground does).
//   node package.mjs compress <file.wasm> ...
//       Write <file.wasm>.br beside each: Brotli quality 11 with a 24-bit
//       window, the same bytes on every run. A release publishes both.
//   node package.mjs kit <version> <assets-dir> <out-dir>
//       Assemble zipp-<version>-wasm-embed-kit/ and its .zip in <out-dir> from
//       the release modules in <assets-dir> (their names: assetName below)
//       and this checkout's loader, host SDK, gpu-lab runtime and docs.
//       Reads ENGINE_DTS (the javascript-python glue's zipp_wasm.d.ts).

import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

const here = path.dirname(fileURLToPath(import.meta.url));
const crate = path.resolve(here, "..");
const repo = path.resolve(crate, "../..");

// The published file name of each module, by variant. Release notes, the
// README and the publish job name the same files.
export const VARIANTS = {
  javascript: "javascript",
  lite: "javascript-lite",
  "javascript-python": "javascript-python",
  "javascript-python-base": "javascript-python-base",
  "torch-package": "torch-package",
};
export const assetName = (version, variant) => `zipp-${version}-${VARIANTS[variant]}.wasm`;
export const kitName = version => `zipp-${version}-wasm-embed-kit`;

const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");

export function brotli(bytes) {
  return zlib.brotliCompressSync(bytes, {
    params: {
      [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
      [zlib.constants.BROTLI_PARAM_LGWIN]: 24,
      [zlib.constants.BROTLI_PARAM_SIZE_HINT]: bytes.length,
    },
  });
}

function compress(...files) {
  for (const file of files) {
    const bytes = fs.readFileSync(file);
    const packed = brotli(bytes);
    if (!zlib.brotliDecompressSync(packed).equals(bytes)) throw new Error(`${file}: Brotli round trip differs`);
    fs.writeFileSync(`${file}.br`, packed);
    console.error(`${path.basename(file)}.br: ${bytes.length} -> ${packed.length} bytes`);
  }
}

// --- WebAssembly sections ---------------------------------------------------

function readLeb(bytes, at) {
  let value = 0, shift = 0, byte;
  do {
    byte = bytes[at++];
    value += (byte & 0x7f) * 2 ** shift;
    shift += 7;
  } while (byte & 0x80);
  return [value, at];
}

function leb(value) {
  const out = [];
  do {
    let byte = value % 128;
    value = Math.floor(value / 128);
    if (value) byte |= 0x80;
    out.push(byte);
  } while (value);
  return Buffer.from(out);
}

function sections(bytes) {
  if (bytes.length < 8 || bytes.readUInt32LE(0) !== 0x6d736100 || bytes.readUInt32LE(4) !== 1) {
    throw new Error("not a WebAssembly 1.0 module");
  }
  const out = [];
  let at = 8;
  while (at < bytes.length) {
    const start = at;
    const id = bytes[at++];
    let size;
    [size, at] = readLeb(bytes, at);
    let name = null;
    if (id === 0) {
      const [len, nameAt] = readLeb(bytes, at);
      name = bytes.subarray(nameAt, nameAt + len).toString("utf8");
    }
    at += size;
    if (at > bytes.length) throw new Error("truncated section");
    out.push({ id, name, raw: bytes.subarray(start, at) });
  }
  return out;
}

// A custom section's contents after its name.
function payloadOf(s) {
  const [, bodyAt] = readLeb(s.raw, 1);
  const [len, nameAt] = readLeb(s.raw, bodyAt);
  return s.raw.subarray(nameAt + len);
}

function customSection(name, payload) {
  const nameBytes = Buffer.from(name, "utf8");
  const body = Buffer.concat([leb(nameBytes.length), nameBytes, payload]);
  return Buffer.concat([Buffer.from([0]), leb(body.length), body]);
}

export function stripZipp(bytes) {
  const kept = sections(bytes).filter(s => !(s.id === 0 && s.name.startsWith("zipp.")));
  return Buffer.concat([bytes.subarray(0, 8), ...kept.map(s => s.raw)]);
}

function addSections(bytes, entries) {
  if (sections(bytes).some(s => s.id === 0 && s.name.startsWith("zipp."))) {
    throw new Error("module already carries zipp.* sections");
  }
  return Buffer.concat([bytes, ...entries.map(([name, payload]) => customSection(name, Buffer.from(payload)))]);
}

async function profileOf(glue, wasm) {
  const url = `data:text/javascript;base64,${Buffer.from(glue).toString("base64")}`;
  const bindings = await import(url);
  await bindings.default({ module_or_path: new WebAssembly.Module(wasm) });
  return JSON.parse(bindings.zippProfile());
}

async function embed(variant, gluePath, input, outDir) {
  if (!(variant in VARIANTS) || variant === "torch-package") throw new Error(`unknown engine variant ${variant}`);
  const glue = fs.readFileSync(gluePath, "utf8");
  const wasm = fs.readFileSync(input);
  const profile = await profileOf(glue, wasm);
  const wantLite = variant === "lite";
  if ((profile.variant === "lite") !== wantLite) throw new Error(`module is not the ${variant} variant (${profile.variant})`);
  const meta = {
    format: 1,
    variant,
    version: profile.version,
    commit: profile.source?.sha ?? null,
    languages: profile.languages,
    glueSha256: sha256(Buffer.from(glue, "utf8")),
  };
  const output = path.join(outDir, assetName(meta.version, variant));
  fs.mkdirSync(outDir, { recursive: true });
  fs.writeFileSync(output, addSections(wasm, [["zipp.meta", JSON.stringify(meta)], ["zipp.glue", glue]]));
  fs.writeFileSync(path.join(path.dirname(gluePath), "PROFILE.json"), JSON.stringify(profile, null, 2) + "\n");
  console.error(`${path.basename(output)}: ${variant} ${meta.version} ${meta.commit}, ${wasm.length} + ${fs.statSync(output).size - wasm.length} bytes`);
  console.log(output);
}

function embedTorch(version, commit, input, outDir) {
  const wasm = fs.readFileSync(input);
  const meta = { format: 1, variant: "torch-package", version, commit: commit || null, languages: ["python"] };
  const output = path.join(outDir, assetName(version, "torch-package"));
  fs.mkdirSync(outDir, { recursive: true });
  fs.writeFileSync(output, addSections(wasm, [["zipp.meta", JSON.stringify(meta)]]));
  console.error(`${path.basename(output)}: torch-package ${version} ${meta.commit}`);
  console.log(output);
}

function extract(input, outDir) {
  const bytes = fs.readFileSync(input);
  const found = name => sections(bytes).filter(s => s.id === 0 && s.name === name);
  const [meta] = found("zipp.meta");
  const [glue] = found("zipp.glue");
  if (!meta || !glue) throw new Error(`${input} carries no zipp.meta/zipp.glue sections`);
  fs.mkdirSync(outDir, { recursive: true });
  fs.writeFileSync(path.join(outDir, "zipp_wasm.js"), payloadOf(glue));
  fs.writeFileSync(path.join(outDir, "zipp_wasm_bg.wasm"), stripZipp(bytes));
  fs.writeFileSync(path.join(outDir, "meta.json"), JSON.stringify(JSON.parse(payloadOf(meta).toString("utf8")), null, 2) + "\n");
}

// --- A deterministic zip ------------------------------------------------------

const crcTable = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(bytes) {
  let c = 0xffffffff;
  for (const b of bytes) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

// Entries in name order, all stamped 1980-01-01 00:00, files 0644: the same
// tree always gives the same bytes.
// `date` stamps every entry (local time, as zip stores it); without it, the
// fixed 1980-01-01 makes the same files pack to the same bytes. A website
// needs a real date: a server sends the extracted files' times as
// Last-Modified, and a 1980 one lets caches keep them for years.
export function writeZip(file, entries, { date } = {}) {
  const dosTime = date ? (date.getHours() << 11) | (date.getMinutes() << 5) | (date.getSeconds() >> 1) : 0;
  const dosDate = date ? ((date.getFullYear() - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate() : 0x21;
  const locals = [], centrals = [];
  let offset = 0;
  for (const { name, data } of [...entries].sort((a, b) => (a.name < b.name ? -1 : 1))) {
    const nameBytes = Buffer.from(name, "utf8");
    const packed = zlib.deflateRawSync(data, { level: 9 });
    const stored = packed.length >= data.length;
    const body = stored ? data : packed;
    const crc = crc32(data);
    const head = Buffer.alloc(30);
    head.writeUInt32LE(0x04034b50, 0);
    head.writeUInt16LE(20, 4);
    head.writeUInt16LE(0x0800, 6); // UTF-8 names
    head.writeUInt16LE(stored ? 0 : 8, 8);
    head.writeUInt16LE(dosTime, 10);
    head.writeUInt16LE(dosDate, 12);
    head.writeUInt32LE(crc, 14);
    head.writeUInt32LE(body.length, 18);
    head.writeUInt32LE(data.length, 22);
    head.writeUInt16LE(nameBytes.length, 26);
    locals.push(head, nameBytes, body);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(0x0314, 4); // made by Unix, zip 2.0
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0x0800, 8);
    central.writeUInt16LE(stored ? 0 : 8, 10);
    central.writeUInt16LE(dosTime, 12);
    central.writeUInt16LE(dosDate, 14);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(body.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    central.writeUInt32LE((0o100644 << 16) >>> 0, 38);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, nameBytes);
    offset += head.length + nameBytes.length + body.length;
  }
  const dir = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(dir.length, 12);
  end.writeUInt32LE(offset, 16);
  fs.writeFileSync(file, Buffer.concat([...locals, dir, end]));
}

// --- The embed kit --------------------------------------------------------------

function filesUnder(dir, prefix) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    const rel = `${prefix}/${entry.name}`;
    if (entry.isDirectory()) out.push(...filesUnder(full, rel));
    else out.push([rel, full]);
  }
  return out;
}

function kit(version, assetsDir, outDir) {
  const engineDts = process.env.ENGINE_DTS;
  if (!engineDts) throw new Error("set ENGINE_DTS to the javascript-python glue's zipp_wasm.d.ts");
  const modules = [];
  let commit = null;
  for (const variant of Object.keys(VARIANTS)) {
    const name = assetName(version, variant);
    const bytes = fs.readFileSync(path.join(assetsDir, name));
    const [meta] = sections(bytes).filter(s => s.id === 0 && s.name === "zipp.meta");
    if (!meta) throw new Error(`${name} has no zipp.meta record`);
    const record = JSON.parse(payloadOf(meta).toString("utf8"));
    if (record.version !== version || record.variant !== variant) {
      throw new Error(`${name} records ${record.variant} ${record.version}`);
    }
    commit ??= record.commit;
    if (record.commit !== commit) throw new Error(`${name} is from ${record.commit}, not ${commit}`);
    const entry = { file: name, variant, languages: record.languages, bytes: bytes.length, sha256: sha256(bytes) };
    // The Brotli copy a release publishes beside it, when present: it must
    // decompress to exactly this module.
    const brPath = path.join(assetsDir, `${name}.br`);
    if (fs.existsSync(brPath)) {
      const packed = fs.readFileSync(brPath);
      if (!zlib.brotliDecompressSync(packed).equals(bytes)) throw new Error(`${name}.br does not decompress to ${name}`);
      entry.brotli = { file: `${name}.br`, bytes: packed.length, sha256: sha256(packed) };
    }
    modules.push(entry);
  }
  const top = kitName(version);
  const files = [
    ["README.md", path.join(here, "README.md")],
    ["zipp.mjs", path.join(here, "zipp.mjs")],
    ["zipp.d.ts", path.join(here, "zipp.d.ts")],
    ["zipp-engine.d.ts", engineDts],
    ["LICENSE-APACHE", path.join(repo, "LICENSE-APACHE")],
    ["docs/zipp-wasm.md", path.join(crate, "README.md")],
    ["docs/TORCH_COMPATIBILITY.md", path.join(repo, "docs/python/TORCH_COMPATIBILITY.md")],
    ["host-sdk/README.md", path.join(crate, "host-sdk/README.md")],
    ["host-sdk/zipp-host.mjs", path.join(crate, "host-sdk/zipp-host.mjs")],
    ["host-sdk/zipp-host.worker.mjs", path.join(crate, "host-sdk/zipp-host.worker.mjs")],
    ["gpu-lab/LICENSE", path.join(crate, "gpu-lab/LICENSE")],
    ["gpu-lab/NOTICE", path.join(crate, "gpu-lab/NOTICE")],
    ["gpu-lab/wasm/kernels.wasm", path.join(crate, "gpu-lab/wasm/kernels.wasm")],
    ...filesUnder(path.join(crate, "gpu-lab/src"), "gpu-lab/src"),
    ...filesUnder(path.join(here, "examples"), "examples"),
  ];
  const entries = files.map(([name, from]) => {
    let data = fs.readFileSync(from);
    // Examples name the release's files; the checked-in copies say VERSION.
    if (name.startsWith("examples/") || name === "README.md") {
      data = Buffer.from(data.toString("utf8").replaceAll("{{VERSION}}", version), "utf8");
    }
    return { name, data };
  });
  const sdk = fs.readFileSync(path.join(crate, "host-sdk/zipp-host.mjs"), "utf8").match(/ZIPP_HOST_SDK_VERSION = "([^"]+)"/)?.[1];
  const loader = fs.readFileSync(path.join(here, "zipp.mjs"), "utf8").match(/ZIPP_LOADER_VERSION = "([^"]+)"/)?.[1];
  entries.push({ name: "modules.json", data: Buffer.from(JSON.stringify({ version, commit, modules }, null, 2) + "\n") });
  entries.push({
    name: "BUILD-INFO.txt",
    // KIT_SOURCE names the commit the kit's own files came from when that is
    // not the modules' (a kit rebuilt for a published release).
    data: Buffer.from(
      `version=${version}\ncommit=${commit}\n` +
        (process.env.KIT_SOURCE ? `kit-source=${process.env.KIT_SOURCE}\n` : "") +
        `loader=${loader}\nhost-sdk=${sdk}\n`,
    ),
  });
  const sums = entries
    .map(e => e.name)
    .sort()
    .map(name => `${sha256(entries.find(e => e.name === name).data)}  ${name}\n`)
    .join("");
  entries.push({ name: "SHA256SUMS", data: Buffer.from(sums) });
  const dir = path.join(outDir, top);
  fs.rmSync(dir, { recursive: true, force: true });
  for (const { name, data } of entries) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), data);
  }
  writeZip(path.join(outDir, `${top}.zip`), entries.map(e => ({ name: `${top}/${e.name}`, data: e.data })));
  console.log(`${top}.zip: ${entries.length} files, commit ${commit}`);
}

const [command, ...args] = process.argv.slice(2);
const commands = {
  embed: () => embed(...args),
  compress: () => compress(...args),
  "embed-torch": () => embedTorch(...args),
  strip: () => fs.writeFileSync(args[1], stripZipp(fs.readFileSync(args[0]))),
  extract: () => extract(...args),
  kit: () => kit(...args),
};
if (import.meta.url === `file://${process.argv[1]}` || fileURLToPath(import.meta.url) === path.resolve(process.argv[1] ?? "")) {
  if (!commands[command]) {
    console.error("usage: package.mjs embed|embed-torch|strip|extract|kit ... (see the header)");
    process.exit(2);
  }
  await commands[command]();
}
