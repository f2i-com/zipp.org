// node examples/node.mjs [module.wasm]
// Runs JavaScript (and Python, with a -python module) in a ZIPP engine.
import { loadZipp } from "../zipp.mjs";

const wasm = process.argv[2] ?? new URL("../zipp-{{VERSION}}-javascript.wasm", import.meta.url);
const zipp = await loadZipp(wasm);
console.log(`ZIPP ${zipp.meta.version} (${zipp.meta.variant}): ${zipp.languages.join(", ")}`);

console.log(zipp.run("console.log([1, 2, 3].map(n => n * n).join(' '))").output);

if (zipp.languages.includes("python")) {
  console.log(zipp.run("print(sum(n * n for n in range(10)))", { language: "python" }).output);
}

// A sandbox: nothing permitted unless granted, a step and a time limit, its
// own worker thread. Functions keep their state between calls.
const sandbox = await zipp.createSandbox({
  limits: { instructions: 10_000_000, timeoutMs: 2000 },
  permissions: { files: { "names.txt": "Ada\nGrace" } },
});
const ran = await sandbox.run(`
  let total = 0;
  function add(n) { total += n; return total; }
  console.log(host.callSync("fs.read", "names.txt").split("\\n").join(" & "));
`);
console.log(ran.output);
await sandbox.call("add", [2]);
console.log("add(3) =", (await sandbox.call("add", [3])).value);
await sandbox.close();