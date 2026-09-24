// Preload for the Node lanes (`node --import ./tests/node/torch-plugin-preload.mjs
// tests/node/<suite>.cjs`): adds the torch package (dist/torch/zipp_torch.wasm,
// through its own loader) to the engine the suites load from ./pkg, before
// they run. With the `python` artifact in ./pkg the torch suites then run
// through the package; `python-plugin.cjs` compares their output with `all`.
//
//   ZIPP_TORCH_DIR   where zipp_torch.wasm and zipp_torch.js are (default
//                    ../../dist/torch)
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
// The engine the suites load (pkg-redirect.cjs points ./pkg elsewhere).
const zipp = require(path.join(here, "pkg", "zipp_wasm.js"));
const dir = process.env.ZIPP_TORCH_DIR ?? path.join(here, "..", "..", "dist", "torch");
const { addTorchSync } = await import(pathToFileURL(path.join(dir, "zipp_torch.js")).href);
addTorchSync(zipp, fs.readFileSync(path.join(dir, "zipp_torch.wasm")));
