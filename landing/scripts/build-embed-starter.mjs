#!/usr/bin/env node
// Builds public/embed/zipp-starter-<release>.zip, the starter project the home
// page's "Embed ZIPP on your site" section offers: an index.html, a main.mjs
// that runs the section's default-deny example with loading and error states,
// and a README with the exact download and serve commands. It holds neither
// the engine nor zipp.mjs; the README fetches both from the release.
//
//   node scripts/build-embed-starter.mjs           write the ZIP
//   node scripts/build-embed-starter.mjs --check   fail unless the ZIP holds exactly these files
//
// The output is deterministic: entries in name order, LF line endings (template
// literals normalise a CRLF checkout), and writeZip's fixed timestamps and modes.
// The check compares the unpacked entries rather than the bytes, since deflate
// output may differ between Node's zlib builds.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import zlib from 'node:zlib'
import { writeZip } from '../../crates/zipp-wasm/embed-kit/package.mjs'
import {
  DEFAULT_ENGINE, EMBED_CSP, EMBED_RELEASE, ENGINES, KIT, PYTHON_BASE, RELEASE_TAG_URL, STARTER, TORCH_PACKAGE,
  defaultOutput, defaultProgram, formatMegabytes, releaseAssetUrl,
} from '../src/embed-data.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))
export const STARTER_ZIP = path.resolve(here, '..', 'public', STARTER.path.slice(1))

const engine = DEFAULT_ENGINE
const lite = ENGINES.find((e) => e.id === 'lite')
const python = ENGINES.find((e) => e.id === 'javascript-python')
const size = (asset) => `${formatMegabytes(asset.bytes)}; ${formatMegabytes(asset.brBytes)} as \`.wasm.br\``

const indexHtml = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<!-- zipp.mjs imports the engine's bindings from a blob: URL, compiles the
     WebAssembly module, and starts each sandbox's Worker from itself. -->
<meta http-equiv="Content-Security-Policy" content="default-src 'self'; ${EMBED_CSP}">
<meta name="color-scheme" content="light dark">
<title>ZIPP ${EMBED_RELEASE} starter</title>
<script type="module" src="./main.mjs"></script>
</head>
<body>
<main>
  <h1>ZIPP ${EMBED_RELEASE} starter</h1>
  <p>This page runs one JavaScript program in a ZIPP sandbox that permits nothing:
    no storage, no files, no network and no host functions.</p>
  <!-- main.mjs replaces this line as soon as it runs. Most browsers will not
       run a module from a file:// page at all, so the line says what to do. -->
  <p id="status" role="status" data-state="loading">Starting… If this line does not change,
    main.mjs could not run: open this page through a web server, not as a file (README.md, step 2).</p>
  <h2>What the program printed</h2>
  <pre id="output" tabindex="0" aria-label="Program output"></pre>
  <noscript><p>This page needs JavaScript: ZIPP runs in your browser.</p></noscript>
</main>
</body>
</html>
`

const mainMjs = `// ZIPP ${EMBED_RELEASE} starter: run a program in a sandbox that permits nothing.
//
// Put zipp.mjs (from ${KIT.file}) and ${engine.file} beside this file,
// serve the folder over HTTP and open it; README.md has the commands.

const ENGINE = "./${engine.file}";

const status = document.getElementById("status");
const output = document.getElementById("output");
const show = (state, text) => {
  status.dataset.state = state;
  status.textContent = text;
};

async function main() {
  if (location.protocol === "file:") {
    throw new Error("Open this page through a web server, not as a file: run \`python3 -m http.server 8000\` in this folder and visit http://localhost:8000/ (README.md).");
  }

  // A dynamic import, so that a missing zipp.mjs is reported on the page.
  show("loading", "Loading zipp.mjs…");
  let loadZipp;
  try {
    ({ loadZipp } = await import("./zipp.mjs"));
  } catch (error) {
    throw new Error(\`zipp.mjs did not load (\${error.message}). Download it from the embed kit: README.md has the command.\`);
  }

  show("loading", \`Loading \${ENGINE.slice(2)}…\`);
  let zipp;
  try {
    zipp = await loadZipp(new URL(ENGINE, import.meta.url));
  } catch (error) {
    throw new Error(\`The engine did not load (\${error.message}). Download \${ENGINE.slice(2)}: README.md has the command.\`);
  }

  // Nothing is permitted: no storage, files, network or host functions.
  // It runs in its own Worker, stopped after 30 s or 50 million steps.
  show("running", "Running the program in a sandbox…");
  const sandbox = await zipp.createSandbox();
  try {
    const { output: lines } = await sandbox.run(\`${defaultProgram(engine)}\`);
    output.textContent = lines.join("\\n");
    console.log(lines.join("\\n"));
  } finally {
    await sandbox.close();
  }
  show("done", \`Done: ZIPP \${zipp.meta.version}, the \${zipp.meta.variant} engine.\`);
}

main().catch((error) => {
  // A ZippSandboxError also says why the sandbox stopped (error.category).
  const why = error.category ? \` (\${error.category})\` : "";
  show("error", \`\${error.message}\${why}\`);
  console.error(error);
});
`

const url = releaseAssetUrl
const readme = `# ZIPP ${EMBED_RELEASE} starter

A web page that runs a program in a ZIPP sandbox, with no build step and no
dependencies. It needs two files from the [ZIPP ${EMBED_RELEASE} release](${RELEASE_TAG_URL})
beside it, which this folder does not include:

- \`zipp.mjs\`, the loader, from \`${KIT.file}\`;
- \`${engine.file}\`, the JavaScript engine (${size(engine)}).

## 1. Download the two files

In this folder, on macOS or Linux:

\`\`\`sh
curl -fLO ${url(KIT.file)}
unzip -j ${KIT.file} ${KIT.folder}/${KIT.loader}
curl -fLO ${url(engine.file)}
\`\`\`

On Windows 10 or later, in PowerShell or Command Prompt (\`curl.exe\` and
\`tar\` come with Windows; in PowerShell, \`curl\` alone is another command):

\`\`\`powershell
curl.exe -fLO ${url(KIT.file)}
tar -xf ${KIT.file} --strip-components=1 ${KIT.folder}/${KIT.loader}
curl.exe -fLO ${url(engine.file)}
\`\`\`

The folder then holds \`index.html\`, \`main.mjs\`, \`${KIT.loader}\` and
\`${engine.file}\`; the kit's \`.zip\` can go. The release's
[\`SHA256SUMS\`](${url('SHA256SUMS')}) lists both downloads' checksums.

## 2. Serve the folder

Browsers do not run this page from a \`file://\` address. Serve the folder
over HTTP with either of these, and open the address it prints:

\`\`\`sh
python3 -m http.server 8000     # then open http://localhost:8000/ (Windows: py -m http.server 8000)
npx serve .                     # then open http://localhost:3000/
\`\`\`

## 3. What you should see

The page, and the browser's console, show what the program printed:

\`\`\`text
${defaultOutput(engine).join('\n')}
\`\`\`

\`fetch\` is undefined because the sandbox was given no network permission.
If \`zipp.mjs\` or the engine is missing, or the page was opened as a file,
the page says which.

## Content-Security-Policy

\`index.html\` sets a policy with what ZIPP needs, and a page of yours that
sends one must allow the same: \`zipp.mjs\` imports the engine's bindings from a
\`blob:\` URL and starts each sandbox's Worker from itself.

\`\`\`text
${EMBED_CSP}
\`\`\`

## A smaller download: \`.wasm.br\`

The release also has \`${engine.file}.br\`: the same engine compressed with
Brotli, about a quarter of the size (${engine.brBytes.toLocaleString('en-US')} bytes against
${engine.bytes.toLocaleString('en-US')}). Serve it with \`Content-Encoding: br\` and
\`Content-Type: application/wasm\` and the browser decodes it; \`zipp.mjs\` 1.2.0,
in the kit as published now, also loads a \`.wasm.br\` URL itself, decompressing
it where the runtime can (Node, Firefox) or falling back to the plain \`.wasm\`
beside it. Simple servers such as \`python3 -m http.server\` send no
\`Content-Encoding\`, so this starter loads the plain \`.wasm\`.

## Other engines

Each is one file from the same release; change \`ENGINE\` in \`main.mjs\`.

- \`${lite.file}\` (${size(lite)}): JavaScript without \`Intl\` and \`Temporal\`.
- \`${python.file}\` (${size(python)}): JavaScript and Python, torch built in.
  For Python, also create the sandbox with \`{ language: "python" }\` and give it
  a Python program, such as \`print("6 * 7 =", 6 * 7)\`.
- \`${PYTHON_BASE.file}\` (${formatMegabytes(PYTHON_BASE.bytes)}) with \`${TORCH_PACKAGE.file}\`
  (${formatMegabytes(TORCH_PACKAGE.bytes)}): Python without torch, adding torch only when a program
  needs it: \`loadZipp({ engine: "./${PYTHON_BASE.file}", packages: ["./${TORCH_PACKAGE.file}"] })\`.

## Next

- Permissions (storage, files, a network allowlist, host functions) and
  limits: https://www.zipp.org/wasm/#from-0-0-26
- Switch each one on and off in your browser: https://www.zipp.org/sandbox/playground/
- The kit's own \`README.md\` covers \`zipp.mjs\` in full, Node, Deno and Bun.
`

/** The starter's files, in the order a ZIP lists them. */
export function starterEntries() {
  const files = { 'README.md': readme, 'index.html': indexHtml, 'main.mjs': mainMjs }
  return Object.entries(files)
    .map(([name, text]) => ({ name: `${STARTER.folder}/${name}`, data: Buffer.from(text.replace(/\r\n/g, '\n'), 'utf8') }))
    .sort((a, b) => (a.name < b.name ? -1 : 1))
}

/** Read a ZIP's file entries (names in central-directory order, data inflated). */
export function readZip(buffer) {
  const end = buffer.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]))
  if (end < 0) throw new Error('not a ZIP: no end of central directory')
  const count = buffer.readUInt16LE(end + 10)
  let at = buffer.readUInt32LE(end + 16)
  const entries = []
  for (let i = 0; i < count; i++) {
    if (buffer.readUInt32LE(at) !== 0x02014b50) throw new Error('bad central directory entry')
    const method = buffer.readUInt16LE(at + 10)
    const packed = buffer.readUInt32LE(at + 20)
    const nameLength = buffer.readUInt16LE(at + 28)
    const extraLength = buffer.readUInt16LE(at + 30)
    const commentLength = buffer.readUInt16LE(at + 32)
    const local = buffer.readUInt32LE(at + 42)
    const name = buffer.subarray(at + 46, at + 46 + nameLength).toString('utf8')
    const dataStart = local + 30 + buffer.readUInt16LE(local + 26) + buffer.readUInt16LE(local + 28)
    const body = buffer.subarray(dataStart, dataStart + packed)
    entries.push({ name, method, data: method === 8 ? zlib.inflateRawSync(body) : Buffer.from(body) })
    at += 46 + nameLength + extraLength + commentLength
  }
  return entries
}

function sameEntries(a, b) {
  return a.length === b.length && a.every((entry, i) => entry.name === b[i].name && entry.data.equals(b[i].data))
}

const isMain = process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])
if (isMain) {
  const entries = starterEntries()
  if (process.argv.includes('--check')) {
    const current = fs.existsSync(STARTER_ZIP) ? readZip(fs.readFileSync(STARTER_ZIP)) : []
    if (!sameEntries(current, entries)) {
      console.error(`${path.relative(process.cwd(), STARTER_ZIP)} is out of date: run node scripts/build-embed-starter.mjs`)
      process.exit(1)
    }
    console.log(`${path.basename(STARTER_ZIP)} is up to date (${entries.length} files)`)
  } else {
    fs.mkdirSync(path.dirname(STARTER_ZIP), { recursive: true })
    writeZip(STARTER_ZIP, entries)
    console.log(`${path.relative(process.cwd(), STARTER_ZIP)}: ${entries.map((e) => e.name).join(', ')} (${fs.statSync(STARTER_ZIP).size} bytes)`)
  }
}
