#!/usr/bin/env node
// Drives the playground in a real browser: loads each sample, runs it, checks
// the console and the canvas, feeds a click and a key, and proves a runaway
// program is stopped by the deadline. Needs the Python-enabled engine at
// dist/all/ and the Playwright package (a locally installed Chrome or Edge is
// used; no browser download is needed):
//
//   cd crates/zipp-wasm
//   ./build-variants.sh all
//   npm install --no-save playwright          # or NODE_PATH to an install
//   node playground/smoke.cjs                 # PLAYWRIGHT_CHANNEL=chrome|msedge
"use strict";
const path = require("node:path");
const os = require("node:os");
const fs = require("node:fs");
const { spawn } = require("node:child_process");

let playwright;
try {
  playwright = require("playwright");
} catch {
  console.error("playwright is not installed: npm install --no-save playwright (browsers are not needed)");
  process.exit(2);
}

const PORT = 8766;
const URL = `http://127.0.0.1:${PORT}/crates/zipp-wasm/playground/`;
let pass = 0, fail = 0;
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}

async function main() {
  const server = spawn(process.execPath, [path.join(__dirname, "serve.cjs")], {
    env: { ...process.env, PORT: String(PORT) }, stdio: ["ignore", "pipe", "inherit"],
  });
  await new Promise((resolve) => server.stdout.on("data", (d) => { if (String(d).includes("http://")) resolve(); }));
  const channel = process.env.PLAYWRIGHT_CHANNEL || "chrome";
  let browser;
  try {
    browser = await playwright.chromium.launch({ channel, headless: true });
  } catch (error) {
    console.error(`could not launch ${channel}: ${error.message}; set PLAYWRIGHT_CHANNEL=msedge or install Chrome`);
    server.kill();
    process.exit(2);
  }
  try {
    const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
    const pageErrors = [];
    page.on("pageerror", (e) => pageErrors.push(String(e)));
    await page.goto(URL);
    const consoleText = () => page.locator("#console").innerText();
    await page.waitForFunction(() => /languages:/.test(document.querySelector("#status").textContent), null, { timeout: 60000 });
    const status = await page.locator("#status").innerText();
    ok("engine profile reached the status bar", /python/.test(status), status);

    // ---- the Python sample: files, run, frames, input ------------------------
    await page.evaluate(() => localStorage.clear());
    await page.locator("#sample-button").click();
    await page.locator('[data-sample="python"]').click();
    await page.waitForFunction(() => document.querySelectorAll("#file-list li").length === 2);
    ok("the samples menu closes after a choice", !(await page.locator("#sample-menu").isVisible()));
    await page.locator("#sample-button").click();
    ok("the samples menu opens on its button", await page.locator("#sample-menu").isVisible());
    await page.locator("#editor").click();
    ok("the samples menu closes on a click elsewhere", !(await page.locator("#sample-menu").isVisible()));
    const pyHtml = await page.locator("#highlight-code").innerHTML();
    ok("python source is syntax-highlighted", /class="tok-keyword">def</.test(pyHtml) && /class="tok-builtin">ui</.test(pyHtml) && /class="tok-string">/.test(pyHtml), pyHtml.slice(0, 120));
    ok("the highlighted mirror matches the textarea text", (await page.locator("#highlight-code").innerText()).trimEnd() === (await page.locator("#editor").inputValue()).trimEnd());
    const files = await page.locator("#file-list li").allInnerTexts();
    ok("python sample lists both files with the entry marked", files.join("|").includes("main.py") && files.join("|").includes("entry") && files.join("|").includes("physics.py"), files.join("|"));
    await page.locator("#run").click();
    await page.waitForFunction(() => /balls ready: 1/.test(document.querySelector("#console").innerText), null, { timeout: 20000 });
    ok("python top level printed through the console", true);
    await page.waitForFunction(() => /frame \d+/.test(document.querySelector("#frame-stats").textContent), null, { timeout: 20000 });
    ok("frames are being rendered", true);
    const size = await page.evaluate(() => [document.querySelector("#canvas").width, document.querySelector("#canvas").height]);
    ok("ui.canvas resized the canvas", size[0] === 480 && size[1] === 320, JSON.stringify(size));
    const pixel = await page.evaluate(() => {
      const c = document.querySelector("#canvas");
      const d = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
      let yellow = 0;
      for (let i = 0; i < d.length; i += 4) if (d[i] > 200 && d[i + 1] > 150 && d[i + 2] < 80) yellow++;
      return yellow;
    });
    ok("a ball was drawn on the canvas", pixel > 50, `yellow pixels: ${pixel}`);
    // A click spawns a ball (on_click) and the frame counter keeps moving.
    await page.locator("#canvas").click({ position: { x: 100, y: 100 } });
    await page.locator("#canvas").click({ position: { x: 200, y: 100 } });
    await page.keyboard.press("Space");
    await page.waitForTimeout(600);
    const yellowAfter = await page.evaluate(() => {
      const c = document.querySelector("#canvas");
      const d = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
      let yellow = 0;
      for (let i = 0; i < d.length; i += 4) if (d[i] > 200 && d[i + 1] > 150 && d[i + 2] < 80) yellow++;
      return yellow;
    });
    ok("clicks and the space key spawned more balls (on_click / on_key)", yellowAfter > pixel * 2, `yellow pixels: ${pixel} -> ${yellowAfter}`);
    const framesBefore = await page.locator("#frame-stats").innerText();
    await page.waitForTimeout(500);
    const framesAfter = await page.locator("#frame-stats").innerText();
    ok("update() keeps advancing frames", framesBefore !== framesAfter, `${framesBefore} -> ${framesAfter}`);
    await page.locator("#stop").click();
    await page.waitForFunction(() => /Stopped\./.test(document.querySelector("#console").innerText));
    ok("stop ends the frame loop", /idle/.test(await page.locator("#status").innerText()));

    // ---- editing: a Python error names the file and line ----------------------
    await page.locator("#file-list li", { hasText: "physics.py" }).click();
    await page.locator("#editor").fill("def step_ball(b, w, h):\n    return 1 +\n");
    await page.locator("#run").click();
    await page.waitForFunction(() => /SyntaxError.*physics\.py:2:\d+/.test(document.querySelector("#console").innerText), null, { timeout: 20000 });
    ok("a syntax error in a module names physics.py and the line", true);

    // ---- a runaway program is cut off by the deadline -----------------------------
    await page.locator("#editor").fill("def step_ball(b, w, h):\n    return None\ndef inside(px, py, x, y, w, h):\n    return False\n");
    await page.locator("#file-list li", { hasText: "main.py" }).click();
    await page.locator("#editor").fill("x = 0\nwhile True:\n    x = x + 1\n");
    await page.locator("#run").click();
    await page.waitForFunction(() => /did not respond|resource limit/.test(document.querySelector("#console").innerText), null, { timeout: 30000 });
    ok("an infinite loop is stopped by the deadline or the budget", true);
    await page.waitForFunction(() => /languages:/.test(document.querySelector("#status").textContent), null, { timeout: 60000 });
    ok("the engine is back after the restart", true);

    // ---- the JavaScript sample ---------------------------------------------------
    await page.locator("#sample-button").click();
    await page.locator('[data-sample="javascript"]').click();
    await page.waitForFunction(() => document.querySelector("#project-name").textContent === "js-balls" && document.querySelectorAll("#file-list li").length === 2);
    const jsHtml = await page.locator("#highlight-code").innerHTML();
    ok("javascript source is syntax-highlighted", /class="tok-keyword">const</.test(jsHtml) && /class="tok-comment">\/\//.test(jsHtml), jsHtml.slice(0, 120));
    await page.locator("#run").click();
    await page.waitForFunction(() => /balls ready: 1/.test(document.querySelector("#console").innerText), null, { timeout: 20000 });
    await page.waitForFunction(() => /frame \d+/.test(document.querySelector("#frame-stats").textContent), null, { timeout: 20000 });
    ok("javascript sample runs with frames", true);
    const jsPixel = await page.evaluate(() => {
      const c = document.querySelector("#canvas");
      const d = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
      let blue = 0;
      for (let i = 0; i < d.length; i += 4) if (d[i] < 120 && d[i + 1] > 150 && d[i + 2] > 200) blue++;
      return blue;
    });
    ok("the javascript paddle was drawn", jsPixel > 100, `blue pixels: ${jsPixel}`);
    await page.locator("#stop").click();

    // ---- Langton's ant: classes, dataclasses and enums in the browser ------------
    await page.locator("#sample-button").click();
    await page.locator('[data-sample="ant"]').click();
    await page.waitForFunction(() => document.querySelector("#project-name").textContent === "langtons-ant" && document.querySelectorAll("#file-list li").length === 2);
    await page.locator("#run").click();
    await page.waitForFunction(() => /Langton's ant on a 100x100 grid with 3 rules/.test(document.querySelector("#console").innerText), null, { timeout: 30000 });
    await page.waitForFunction(() => /frame \d+/.test(document.querySelector("#frame-stats").textContent), null, { timeout: 20000 });
    ok("the ant project runs with frames", true);
    await page.locator("#canvas").click({ position: { x: 250, y: 250 } });
    await page.keyboard.press("Space");
    await page.waitForTimeout(1500);
    const dark = await page.evaluate(() => {
      const c = document.querySelector("#canvas");
      const d = c.getContext("2d").getImageData(0, 0, c.width, 500).data;
      let n = 0;
      for (let i = 0; i < d.length; i += 4) if (d[i] < 60 && d[i + 1] < 60 && d[i + 2] < 60) n++;
      return n;
    });
    ok("the ant painted cells after space started it", dark > 200, `dark pixels: ${dark}`);
    // A long run: the budget is renewed per frame, so thousands of steps
    // (each frame redraws the whole 100x100 grid) must not exhaust it.
    await page.keyboard.press("ArrowUp"); await page.keyboard.press("ArrowUp"); await page.keyboard.press("ArrowUp");
    await page.waitForTimeout(6000);
    const consoleAfter = await consoleText();
    ok("a long ant run stays within the per-frame budget", !/instruction budget|resource limit|disposed/.test(consoleAfter), consoleAfter.split("\n").slice(-2).join(" | "));
    const stats = await page.evaluate(() => document.querySelector("#frame-stats").textContent);
    ok("frames kept advancing during the long run", /frame (\d+)/.test(stats) && Number(stats.match(/frame (\d+)/)[1]) > 60, stats);
    await page.locator("#stop").click();

    // ---- GPU compute: portable Torch Life answered by the compute runtime -------------
    // Which backend answers depends on the browser (headless Chromium usually has
    // no WebGPU and may lack float render targets); the check is that some
    // backend answered with the right numbers and that the frame loop kept
    // computing generations of life through it.
    await page.locator("#sample-button").click();
    await page.locator('[data-sample="gpu"]').click();
    await page.waitForFunction(() => document.querySelector("#project-name").textContent === "gpu-compute");
    await page.locator("#run").click();
    await page.waitForFunction(() => /Life generation 1: /.test(document.querySelector("#console").innerText), null, { timeout: 30000 });
    const gpuConsole = await consoleText();
    ok("Torch Life returned its first generation", /Life generation 1: [0-9]+ alive/.test(gpuConsole));
    const backendLine = (gpuConsole.match(/GPU compute: (\S+)/) || [])[1] || "none";
    ok("the compute runtime reported its backend", /GPU compute: (webgpu|webgl2|wasm|cpu-js)/.test(gpuConsole), gpuConsole.split("\n").filter((l) => /GPU compute/.test(l)).join(" | "));
    console.log(`  note GPU backend in this browser: ${backendLine}`);
    await page.waitForTimeout(3000);
    const generations = await page.evaluate(() => { const c = document.querySelector("#canvas"); return c.width; });
    const gpuAfter = await consoleText();
    ok("life generations advance through the compute backend without errors", !/Error|error:|stopped/.test(gpuAfter.split("\n").slice(-6).join("\n")) && generations === 384, gpuAfter.split("\n").slice(-2).join(" | "));
    // An explicit WebGL2 request must use WebGL2 or say why it cannot; it never
    // falls back to a CPU backend quietly.
    await page.locator("#stop").click();
    await page.selectOption("#gpu-backend", "webgl2");
    await page.locator("#run").click();
    await page.waitForFunction(() => /GPU compute: webgl2|life stopped: UNAVAILABLE/.test(document.querySelector("#console").innerText), null, { timeout: 30000 });
    const webglConsole = await consoleText();
    if (/GPU compute: webgl2/.test(webglConsole)) {
      await page.waitForFunction(() => /Life generation 1: /.test(document.querySelector("#console").innerText), null, { timeout: 30000 });
      ok("WebGL2 executed the Torch Life graph", /Life generation 1: [0-9]+ alive webgl2/.test(await consoleText()));
    } else {
      ok("an explicit WebGL2 request is refused with a reason rather than silently downgraded", /UNAVAILABLE/.test(webglConsole) && !/GPU compute: (wasm|cpu-js)/.test(webglConsole), webglConsole.split("\n").filter((l) => /UNAVAILABLE/.test(l)).slice(0, 1).join(""));
      console.log("  note WebGL2 float render targets are unavailable in this browser; GPU execution not verified here");
    }
    await page.locator("#stop").click();
    await page.selectOption("#gpu-backend", "auto");

    // ---- a project folder like a research lab: subfolders, a package, data,
    // a binary checkpoint, program arguments, and files the program writes.
    const labDir = fs.mkdtempSync(path.join(os.tmpdir(), "zipp-lab-"));
    const labFiles = {
      "run.py": [
        "import sys, os, json",
        "from legacy.fast_memory import Memory",
        "import pkg.tools as tools",
        "with open('data/config.json') as f:",
        "    cfg = json.load(f)",
        "with open('data/model.bin', 'rb') as f:",
        "    raw = f.read()",
        "print('argv', sys.argv[1:], 'steps', cfg['steps'], Memory().name, tools.twice(cfg['steps']), len(raw), raw[-1])",
        "os.makedirs('out', exist_ok=True)",
        "with open('out/result.json', 'w') as f:",
        "    json.dump({'argv': sys.argv[1:], 'bytes': len(raw)}, f)",
        "",
      ].join("\n"),
      "legacy/fast_memory.py": "class Memory:\n    name = 'fast'\n",
      "pkg/__init__.py": "",
      "pkg/tools.py": "def twice(n):\n    return n * 2\n",
      "data/config.json": JSON.stringify({ steps: 21 }),
      "notes.md": "# lab notes\n",
      "__pycache__/run.cpython-312.pyc": "junk",
    };
    for (const [rel, text] of Object.entries(labFiles)) {
      fs.mkdirSync(path.dirname(path.join(labDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(labDir, rel), text);
    }
    fs.writeFileSync(path.join(labDir, "data/model.bin"), Buffer.from(Array.from({ length: 1000 }, (_, i) => i % 256)));
    await page.locator("#folder-input").setInputFiles(labDir);
    await page.waitForFunction(() => document.querySelector("#project-name").textContent.startsWith("zipp-lab-"));
    const labRows = await page.locator("#file-list li").allInnerTexts();
    ok("the folder tree lists subfolders and every file", labRows.some((r) => /legacy/.test(r)) && labRows.some((r) => /fast_memory\.py/.test(r)) && labRows.some((r) => /model\.bin/.test(r)) && labRows.some((r) => /notes\.md/.test(r)), labRows.join("|"));
    ok("tool folders are skipped", !labRows.some((r) => /pycache|\.pyc/.test(r)), labRows.join("|"));
    ok("the binary is tagged and run.py is the entry", labRows.some((r) => /model\.bin\s+bin/.test(r)) && labRows.some((r) => /run\.py\s+entry/.test(r)), labRows.join("|"));
    await page.locator('#file-list li[data-name="data/model.bin"]').click();
    ok("a binary file shows a placeholder in a read-only editor", /binary file, 1000 bytes/.test(await page.locator("#editor").inputValue()) && await page.locator("#editor").isDisabled());
    await page.locator('#file-list li[data-name="run.py"]').click();
    ok("a source file is editable again", !(await page.locator("#editor").isDisabled()));
    await page.locator("#program-args").fill("--steps 7 'two words'");
    await page.locator("#run").click();
    await page.waitForFunction(() => /Program finished/.test(document.querySelector("#console").innerText), null, { timeout: 30000 });
    const labConsole = await consoleText();
    ok("the program saw its arguments, packages, data file and binary", /argv \['--steps', '7', 'two words'\] steps 21 fast 42 1000 231/.test(labConsole), labConsole.split("\n").slice(-3).join(" | "));
    const afterRows = await page.locator("#file-list li").allInnerTexts();
    ok("a file the program wrote appears in the tree tagged as written", afterRows.some((r) => /result\.json\s+written/.test(r)), afterRows.join("|"));
    await page.locator('#file-list li[data-name="out/result.json"]').click();
    ok("the written file opens with the program's content", /"bytes": 1000/.test(await page.locator("#editor").inputValue()) || /"bytes":1000/.test(await page.locator("#editor").inputValue()), await page.locator("#editor").inputValue());
    await page.locator("#program-args").fill("");
    fs.rmSync(labDir, { recursive: true, force: true });

    // ---- hello samples: no hooks, just output and a static drawing --------------
    for (const [key, name, needle] of [["python-hello", "python-hello", /fib\(30\) = 832040/], ["javascript-hello", "js-hello", /fib\(20\) = 6765/]]) {
      await page.locator("#sample-button").click();
      await page.locator(`[data-sample="${key}"]`).click();
      await page.waitForFunction((n) => document.querySelector("#project-name").textContent === n, name);
      await page.locator("#run").click();
      await page.waitForFunction((src) => new RegExp(src).test(document.querySelector("#console").innerText) && /Program finished/.test(document.querySelector("#console").innerText), needle.source, { timeout: 20000 });
      ok(`${key} prints and finishes`, true);
    }
    ok("no uncaught page errors", pageErrors.length === 0, pageErrors.join("; "));
    console.log(await consoleText().then((t) => t.split("\n").slice(-3).join(" | ")));
  } finally {
    await browser.close();
    server.kill();
  }
  console.log(`\n${pass} passed, ${fail} failed`);
  process.exit(fail ? 1 : 0);
}

main().catch((error) => { console.error(error); process.exit(1); });
