// The torch package lane: the `python` artifact (no torch built in) plus
// zipp_torch.wasm must behave exactly as the `all` artifact (torch built in).
//
//   node tests/node/python-plugin.cjs <all pkg dir> <python pkg dir> [torch dir]
//
// Both pkg dirs are `wasm-bindgen --target nodejs` outputs; the torch dir holds
// zipp_torch.wasm and zipp_torch.js (default ../../dist/torch). Checks:
//   - without the package, `import torch` raises ModuleNotFoundError (name
//     'torch') saying how the host adds it, and the rest of Python works;
//   - the engine refuses a malformed or tampered archive and registers
//     nothing, and accepts the same archive twice;
//   - with the package, the torch suites (python-conv2d, python-training,
//     python-gpu) print byte for byte what they print against `all`;
//   - the tensor kernels run in the package's module (a 192x192 matmul is
//     far faster than the JavaScript loop the runtime falls back to).
"use strict";
const { spawnSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const { pathToFileURL } = require("url");

const [allPkg, pyPkg, torchArg] = process.argv.slice(2);
if (!allPkg || !pyPkg) {
  console.error("usage: node tests/node/python-plugin.cjs <all pkg dir> <python pkg dir> [torch dir]");
  process.exit(2);
}
const torchDir = path.resolve(torchArg ?? path.join(__dirname, "..", "..", "dist", "torch"));
const here = __dirname;

let pass = 0, fail = 0;
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}

function node(args, pkg, withTorch) {
  const pre = ["-r", path.join(here, "pkg-redirect.cjs")];
  if (withTorch) pre.push("--import", pathToFileURL(path.join(here, "torch-plugin-preload.mjs")).href);
  return spawnSync(process.execPath, [...pre, ...args], {
    cwd: path.join(here, "..", ".."),
    env: { ...process.env, ZIPP_PKG: path.resolve(pkg), ZIPP_TORCH_DIR: torchDir },
    encoding: "utf8",
    maxBuffer: 64 << 20,
  });
}

function script(body, pkg, withTorch) {
  const r = node(["-e", `const zipp = require("./tests/node/pkg/zipp_wasm.js");\n${body}`], pkg, withTorch);
  return { out: r.stdout.trim(), err: r.stderr.trim(), status: r.status };
}

// ---- without the package ----
{
  const r = script(`
    const e = new zipp.Engine();
    let message = "";
    try { e.initSource("import torch\\n", "python"); } catch (err) { message = String(err); }
    console.log(JSON.stringify(message));
    const f = new zipp.Engine();
    f.initSource("try:\\n    import torch.nn\\nexcept ImportError as e:\\n    print(type(e).__name__, e.name)\\nimport pickle, zipfile\\nprint(zipfile.__name__, pickle.loads(pickle.dumps([1, 2])))\\n", "python");
    console.log(JSON.stringify(f.takeOutput()));
    console.log(zipp.pythonPackages());
  `, pyPkg, false);
  const [message, output, packages] = r.out.split("\n");
  ok("python without torch: import torch raises ModuleNotFoundError", /ModuleNotFoundError: No module named 'torch'/.test(message), message);
  ok("the error says how the host adds torch", /addPythonPackage/.test(message) && /zipp_torch\.wasm/.test(message), message);
  ok("it is an ImportError whose name is 'torch'; the rest of the library works",
    output === JSON.stringify(["ModuleNotFoundError torch", "zipfile [1, 2]"]), output);
  ok("pythonPackages: torch not built in, nothing added", JSON.parse(packages ?? "{}").torchBuiltIn === false
    && JSON.parse(packages ?? "{}").installed.length === 0, packages);
}

// ---- archive checks ----
{
  const r = script(`
    const fs = require("fs");
    const x = new WebAssembly.Instance(new WebAssembly.Module(fs.readFileSync(${JSON.stringify(path.join(torchDir, "zipp_torch.wasm"))})), {}).exports;
    const archive = new Uint8Array(x.memory.buffer, x.zipp_package_ptr(), x.zipp_package_len()).slice();
    const tryAdd = (bytes) => { try { zipp.addPythonPackage(bytes, undefined); return "ok"; } catch (e) { return String(e); } };
    const garbage = tryAdd(new TextEncoder().encode("not a package"));
    const tampered = archive.slice(); tampered[tampered.length - 10] ^= 1;
    const bad = tryAdd(tampered);
    const other = archive.slice();
    const at = new TextDecoder().decode(archive.subarray(0, 400)).indexOf("engine-abi ") + 11;
    other.fill(48, at, at + 16);
    const wrongAbi = tryAdd(other);
    console.log(JSON.stringify([garbage, bad, wrongAbi, zipp.pythonPackages()]));
  `, pyPkg, false);
  let got = [];
  try { got = JSON.parse(r.out); } catch { /* reported below */ }
  ok("a malformed archive is refused", /addPythonPackage: not a Python package archive/.test(got[0]), got[0] ?? r.err);
  ok("a tampered file is refused by its SHA-256", /SHA-256 does not match/.test(got[1]), got[1]);
  ok("another engine ABI is refused", /engine ABI 0000000000000000/.test(got[2]), got[2]);
  ok("nothing was registered", JSON.parse(got[3] ?? "{}").installed?.length === 0, got[3]);
}

// ---- with the package: the same as `all` ----
{
  const r = script(`
    const info = zipp.pythonPackages();
    const fs = require("fs");
    const again = (() => { try { const x = new WebAssembly.Instance(new WebAssembly.Module(fs.readFileSync(${JSON.stringify(path.join(torchDir, "zipp_torch.wasm"))})), {}).exports;
      zipp.addPythonPackage(new Uint8Array(x.memory.buffer, x.zipp_package_ptr(), x.zipp_package_len()).slice(), undefined); return "ok"; } catch (e) { return String(e); } })();
    const time = (native) => {
      const e = new zipp.Engine(); e.setInstructionBudget(2e9);
      e.initSource("import torch, time\\nimport _zipp_tensor as k\\nk._native(" + native + ")\\ntorch.manual_seed(0)\\na = torch.randn(192, 192)\\na @ a\\ns = time.perf_counter()\\nfor _ in range(3):\\n    b = a @ a\\nprint((time.perf_counter() - s) / 3 * 1000)\\n", "python");
      return Number(e.takeOutput()[0]);
    };
    console.log(JSON.stringify([info, again, time("True"), time("False")]));
  `, pyPkg, true);
  let got = [];
  try { got = JSON.parse(r.out); } catch { /* reported below */ }
  const info = JSON.parse(got[0] ?? "{}");
  ok("the package is installed", info.installed?.[0]?.name === "torch", got[0] ?? r.err);
  ok("adding the same archive again is accepted", got[1] === "ok", got[1]);
  ok(`the kernels run in the package's module (matmul ${Number(got[2]).toFixed(1)} ms vs ${Number(got[3]).toFixed(1)} ms in JavaScript)`,
    got[2] > 0 && got[2] * 5 < got[3]);
}
for (const suite of ["python-conv2d", "python-training", "python-gpu"]) {
  const file = path.join("tests", "node", `${suite}.cjs`);
  const a = node([file], allPkg, false);
  const b = node([file], pyPkg, true);
  ok(`${suite}: all passes`, a.status === 0, a.stderr.slice(-400));
  ok(`${suite}: python + torch package passes`, b.status === 0, b.stderr.slice(-400));
  ok(`${suite}: the same output byte for byte`, a.stdout === b.stdout && a.stdout.length > 0);
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
