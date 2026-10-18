#!/usr/bin/env node
// node scripts/compress-engines.mjs [--check]
//
// Writes the Brotli copy (.wasm.br: quality 11, 24-bit window, the same bytes
// on every run) beside every engine .wasm the site serves, in public/wasm,
// public/playground-runtime and public/sandbox. public/.htaccess sends a
// browser that accepts Brotli the .br in place of the .wasm, with
// `Content-Encoding: br`. With --check, it only verifies that every .wasm has
// a .br that decompresses to exactly it (and that no stale .br is left), and
// exits nonzero otherwise. Run it after refreshing the engines.
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "public");
const dirs = ["wasm", "playground-runtime", "sandbox"];
const check = process.argv.includes("--check");

function brotli(bytes) {
  return zlib.brotliCompressSync(bytes, {
    params: {
      [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
      [zlib.constants.BROTLI_PARAM_LGWIN]: 24,
      [zlib.constants.BROTLI_PARAM_SIZE_HINT]: bytes.length,
    },
  });
}

let problems = 0;
for (const dir of dirs) {
  const full = path.join(root, dir);
  if (!fs.existsSync(full)) continue;
  const names = fs.readdirSync(full);
  for (const name of names.filter(n => n.endsWith(".wasm"))) {
    const file = path.join(full, name);
    const bytes = fs.readFileSync(file);
    const brFile = `${file}.br`;
    if (check) {
      const ok = fs.existsSync(brFile) && zlib.brotliDecompressSync(fs.readFileSync(brFile)).equals(bytes);
      if (!ok) {
        problems++;
        console.error(`${dir}/${name}.br is missing or does not decompress to ${dir}/${name}`);
      }
      continue;
    }
    const packed = brotli(bytes);
    fs.writeFileSync(brFile, packed);
    console.log(`${dir}/${name}.br: ${bytes.length.toLocaleString("en-US")} -> ${packed.length.toLocaleString("en-US")} bytes`);
  }
  for (const name of names.filter(n => n.endsWith(".wasm.br") && !names.includes(n.slice(0, -3)))) {
    if (check) {
      problems++;
      console.error(`${dir}/${name} has no .wasm beside it`);
    } else {
      fs.rmSync(path.join(full, name));
      console.log(`removed stale ${dir}/${name}`);
    }
  }
}
if (check) {
  if (problems) process.exit(1);
  console.log("every engine .wasm has a matching .wasm.br");
}
