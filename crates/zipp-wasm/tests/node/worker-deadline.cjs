// Prove the deployment's load-bearing wall-clock boundary: the deadline runs
// outside the Worker executing synchronous WASM, terminates that entire Worker,
// and a fresh Worker/WASM instance serves the next tenant.
const assert = require("node:assert/strict");
const { Worker, isMainThread, parentPort } = require("node:worker_threads");

if (!isMainThread) {
  const { Engine } = require("./pkg/zipp_wasm.js");
  const engine = new Engine();
  parentPort.postMessage({ type: "ready" });
  parentPort.on("message", ({ type }) => {
    if (type === "runaway") {
      parentPort.postMessage({ type: "started" });
      try {
        engine.initScript("for (;;) {}");
        parentPort.postMessage({ type: "unexpected-completion" });
      } catch {
        parentPort.postMessage({ type: "unexpected-completion" });
      }
      return;
    }
    if (type === "healthy") {
      engine.initScript("function answer() { return 42; }");
      parentPort.postMessage({ type: "answer", value: engine.callFunction("answer", []) });
      parentPort.close();
    }
  });
  return;
}

function waitFor(worker, wanted, timeoutMs = 5_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${wanted}`)), timeoutMs);
    const onMessage = (message) => {
      if (message.type !== wanted) return;
      clearTimeout(timer);
      worker.off("message", onMessage);
      resolve(message);
    };
    worker.on("message", onMessage);
    worker.once("error", reject);
  });
}

async function terminateWithin(worker, timeoutMs) {
  let timer;
  try {
    await Promise.race([
      worker.terminate(),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("Worker.terminate() did not complete")),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function main() {
  const runaway = new Worker(__filename);
  await waitFor(runaway, "ready");
  let completed = false;
  runaway.on("message", (message) => {
    if (message.type === "unexpected-completion") completed = true;
  });
  runaway.postMessage({ type: "runaway" });
  await waitFor(runaway, "started");

  const began = Date.now();
  await new Promise((resolve) => setTimeout(resolve, 25));
  await terminateWithin(runaway, 2_000);
  assert.equal(completed, false, "runaway WASM returned before external termination");
  assert.ok(Date.now() - began < 2_000, "external deadline was not promptly enforceable");

  const replacement = new Worker(__filename);
  await waitFor(replacement, "ready");
  replacement.postMessage({ type: "healthy" });
  const answer = await waitFor(replacement, "answer");
  assert.equal(answer.value, 42, "fresh Worker/WASM instance did not isolate the next tenant");
  await replacement.terminate();
  console.log("ok external Worker deadline and fresh-instance recovery");
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
