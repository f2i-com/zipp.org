// `node -r ./tests/node/pkg-redirect.cjs <suite>.cjs` with ZIPP_PKG=<dir>:
// the suite's `require("./pkg/zipp_wasm.js")` loads <dir>/zipp_wasm.js
// instead, so one suite runs against two builds side by side
// (python-plugin.cjs). Without ZIPP_PKG nothing changes.
"use strict";
const Module = require("module");
const path = require("path");

const target = process.env.ZIPP_PKG;
if (target) {
  const resolve = Module._resolveFilename;
  Module._resolveFilename = function (request, ...rest) {
    if (/(^|[\\/])pkg[\\/]zipp_wasm\.js$/.test(request)) return path.resolve(target, "zipp_wasm.js");
    return resolve.call(this, request, ...rest);
  };
}
