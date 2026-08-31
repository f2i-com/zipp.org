#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import zlib from "node:zlib";

const inputs = process.argv.slice(2);
if (inputs.length === 0) {
  console.error("usage: node measure_wasm.mjs module.wasm [...]");
  process.exit(2);
}

const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");
const artifacts = inputs.map((input) => {
  const modulePath = path.resolve(input);
  const bytes = fs.readFileSync(modulePath);
  const gzip = zlib.gzipSync(bytes, { level: 9 });
  const brotli = zlib.brotliCompressSync(bytes, {
    params: {
      [zlib.constants.BROTLI_PARAM_MODE]: zlib.constants.BROTLI_MODE_GENERIC,
      [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
    },
  });
  return {
    path: modulePath,
    sha256: sha256(bytes),
    raw_bytes: bytes.length,
    gzip_9_bytes: gzip.length,
    brotli_11_bytes: brotli.length,
  };
});

console.log(
  JSON.stringify(
    {
      node: process.version,
      zlib: process.versions.zlib,
      brotli: process.versions.brotli,
      artifacts,
    },
    null,
    2,
  ),
);
