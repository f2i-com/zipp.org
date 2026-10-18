#!/usr/bin/env node
// node scripts/package-site.mjs <out.zip>
//
// Packs the built site (`npm run build` first) into one zip for a PHP host
// such as the Apache origin behind www.zipp.org: dist/client's files at the
// zip's root, ready to extract over the web root. That is the pages, the
// playgrounds and their engines (with the .wasm.br copies .htaccess serves),
// the documentation pages, the journal and api/stats.php. Left out are
// dist/server (the build's prerender bundle) and .assetsignore (read only by
// Cloudflare's Worker deploy). The zip has forward-slash names, and every
// entry is stamped with the packing time: the host serves the extracted
// files' times as Last-Modified, and a fixed 1980 date would let browsers
// and CDNs keep them for years (see public/.htaccess's cache rules).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { writeZip } from "../../crates/zipp-wasm/embed-kit/package.mjs";

const out = process.argv[2];
if (!out) {
  console.error("usage: node scripts/package-site.mjs <out.zip>");
  process.exit(2);
}
const client = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "dist", "client");
const skip = new Set([".assetsignore"]);

function walk(dir, prefix = "") {
  const entries = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const name = prefix + entry.name;
    if (skip.has(name)) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) entries.push(...walk(full, `${name}/`));
    else if (entry.isFile()) entries.push({ name, data: fs.readFileSync(full) });
  }
  return entries;
}

for (const required of ["index.html", "404.html", ".htaccess", "api/stats.php", "sandbox/playground/index.html", "playground/index.html"]) {
  if (!fs.existsSync(path.join(client, required))) {
    console.error(`dist/client/${required} is missing: run npm run build first`);
    process.exit(1);
  }
}
const entries = walk(client);
writeZip(path.resolve(out), entries, { date: new Date() });
const bytes = entries.reduce((sum, e) => sum + e.data.length, 0);
console.log(`${path.basename(out)}: ${entries.length} files, ${bytes.toLocaleString("en-US")} bytes unpacked, ${fs.statSync(out).size.toLocaleString("en-US")} packed`);
