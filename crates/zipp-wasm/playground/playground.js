// The Zipp playground page: a project of .py or .js files in the sidebar, an
// editor, and a canvas + console fed by the engine running in
// engine.worker.js. See README.md for the program contract (`ui`, `draw`,
// `update`, `on_click`, `on_key`).
"use strict";

const DEADLINE_MS = 5000;          // a reply later than this restarts the engine
const INSTRUCTION_BUDGET = 2e9;    // the engine's maximum lifetime budget
const STORAGE_KEY = "zipp-playground-project";
const MAX_CONSOLE_LINES = 2000;
const SOURCE_EXTENSIONS = { py: "python", js: "javascript", mjs: "javascript" };
const SAMPLES = {
  python: { name: "python-balls", entry: "main.py", base: "../../../examples/python/project/", files: ["main.py", "physics.py"] },
  javascript: { name: "js-balls", entry: "main.js", base: "../../../examples/js/project/", files: ["physics.js", "main.js"] },
  ant: { name: "langtons-ant", entry: "main.py", base: "../../../examples/python/langtons_ant/", files: ["main.py", "rules.py"] },
  "python-hello": { name: "python-hello", entry: "main.py", inline: { "main.py": 'import ui\n\ndef fib(n):\n    a = 0\n    b = 1\n    for i in range(n):\n        a, b = b, a + b\n    return a\n\nprint("fib(30) =", fib(30))\n\nui.canvas(320, 120)\nui.clear("#10141c")\nui.font(28)\nui.text(24, 70, "hello from Python", "#ffcc00")\n' } },
  "javascript-hello": { name: "js-hello", entry: "main.js", inline: { "main.js": 'const fib = (n) => (n < 2 ? n : fib(n - 1) + fib(n - 2));\nconsole.log("fib(20) =", fib(20));\n\nui.canvas(320, 120);\nui.clear("#10141c");\nui.font(28);\nui.text(24, 70, "hello from JavaScript", "#58a6ff");\n' } },
};

const $ = (selector) => document.querySelector(selector);
const el = {
  fileList: $("#file-list"), projectName: $("#project-name"), projectLanguage: $("#project-language"),
  currentFile: $("#current-file"), editor: $("#editor"), gutter: $("#gutter"),
  highlight: $("#highlight"), highlightCode: $("#highlight-code"),
  canvas: $("#canvas"), canvasHint: $("#canvas-hint"), consoleOut: $("#console"),
  status: $("#status"), frameStats: $("#frame-stats"), run: $("#run"), stop: $("#stop"),
  dropOverlay: $("#drop-overlay"), sampleMenu: $("#sample-menu"),
};
const ctx = el.canvas.getContext("2d");

// ---- project state ---------------------------------------------------------
const project = { name: "untitled", files: new Map(), entry: null, current: null };

function extensionOf(name) {
  const dot = name.lastIndexOf(".");
  return dot < 0 ? "" : name.slice(dot + 1).toLowerCase();
}
function languageOf(name) {
  return name ? SOURCE_EXTENSIONS[extensionOf(name)] || null : null;
}
function stemOf(name) {
  const dot = name.lastIndexOf(".");
  return dot < 0 ? name : name.slice(0, dot);
}
function projectLanguage() {
  return languageOf(project.entry);
}
function pickEntry() {
  const names = [...project.files.keys()];
  if (project.entry && project.files.has(project.entry)) return;
  project.entry = names.find((n) => n === "main.py") || names.find((n) => n === "main.js")
    || names.find((n) => languageOf(n)) || names[0] || null;
}
function save() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({
      name: project.name, entry: project.entry, current: project.current, files: [...project.files],
    }));
  } catch { /* storage unavailable: edits live for the session only */ }
}
function restore() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return false;
    const saved = JSON.parse(raw);
    if (!Array.isArray(saved.files) || saved.files.length === 0) return false;
    setProject(saved.name || "untitled", saved.files, saved.entry, saved.current);
    return true;
  } catch {
    return false;
  }
}
function setProject(name, entries, entry, current) {
  project.name = name;
  project.files = new Map(entries);
  project.entry = entry && project.files.has(entry) ? entry : null;
  pickEntry();
  project.current = current && project.files.has(current) ? current : project.entry;
  renderFiles();
  openFile(project.current);
  save();
}

// ---- sidebar + editor ------------------------------------------------------
function renderFiles() {
  const language = projectLanguage();
  el.projectName.textContent = project.name;
  el.projectName.title = project.name;
  el.projectLanguage.textContent = language || "";
  el.fileList.replaceChildren();
  for (const name of [...project.files.keys()].sort()) {
    const li = document.createElement("li");
    li.dataset.name = name;
    li.textContent = name;
    if (name === project.current) li.classList.add("active");
    const fileLanguage = languageOf(name);
    if (language && fileLanguage !== language) {
      li.classList.add("ignored");
      li.title = `not run: the project language is ${language}`;
    }
    if (name === project.entry) {
      const tag = document.createElement("span");
      tag.className = "entry";
      tag.textContent = "entry";
      li.append(" ", tag);
    }
    li.addEventListener("click", () => openFile(name));
    el.fileList.append(li);
  }
  $("#set-entry").disabled = !project.current || project.current === project.entry || !languageOf(project.current);
  $("#rename-file").disabled = !project.current;
  $("#delete-file").disabled = !project.current;
}
function openFile(name) {
  project.current = name;
  el.currentFile.textContent = name || "no file";
  el.editor.value = name ? project.files.get(name) : "";
  el.editor.disabled = !name;
  updateGutter();
  for (const li of el.fileList.children) li.classList.toggle("active", li.dataset.name === name);
  renderFiles();
  save();
}
function updateGutter() {
  const lines = el.editor.value.split("\n").length;
  el.gutter.textContent = Array.from({ length: lines }, (_, i) => i + 1).join("\n");
  syncScroll();
  updateHighlight();
}
function syncScroll() {
  el.gutter.scrollTop = el.editor.scrollTop;
  el.highlight.scrollTop = el.editor.scrollTop;
  el.highlight.scrollLeft = el.editor.scrollLeft;
}
el.editor.addEventListener("input", () => {
  if (project.current) project.files.set(project.current, el.editor.value);
  updateGutter();
  save();
});
el.editor.addEventListener("scroll", syncScroll);

// ---- syntax highlighting: a small tokenizer per language, rendered into the
// mirror `pre` behind the textarea. Groups: comment, string, number, word,
// punctuation; words are classified by the language's keyword/builtin sets.
const HIGHLIGHT = {
  python: {
    pattern: /(#[^\n]*)|("""[\s\S]*?"""|'''[\s\S]*?'''|"(?:\\.|[^"\\\n])*"?|'(?:\\.|[^'\\\n])*'?)|(\b\d[\w.]*\b)|(@[A-Za-z_]\w*)|([A-Za-z_]\w*)|([^\sA-Za-z_\d]+)/g,
    keywords: new Set("def return if elif else while for in break continue pass import from as and or not is True False None assert lambda class try except finally raise with yield del global nonlocal".split(" ")),
    builtins: new Set("print len range str int bool list tuple abs min max ui self".split(" ")),
  },
  javascript: {
    pattern: /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|(`(?:\\.|[^`\\])*`?|"(?:\\.|[^"\\\n])*"?|'(?:\\.|[^'\\\n])*'?)|(\b\d[\w.]*\b)|(@[A-Za-z_$][\w$]*)|([A-Za-z_$][\w$]*)|([^\sA-Za-z_$\d]+)/g,
    keywords: new Set("const let var function return if else for while do break continue switch case default new this class extends super import export from of in typeof instanceof void delete throw try catch finally async await yield true false null undefined static get set".split(" ")),
    builtins: new Set("console ui Math JSON Object Array String Number Boolean Map Set Promise Error Date window document".split(" ")),
  },
};
function escapeHtml(text) {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
function highlightSource(source, language) {
  const rules = HIGHLIGHT[language];
  if (!rules) return escapeHtml(source);
  const out = [];
  let last = 0;
  rules.pattern.lastIndex = 0;
  let m;
  while ((m = rules.pattern.exec(source)) !== null) {
    if (m.index > last) out.push(escapeHtml(source.slice(last, m.index)));
    const text = escapeHtml(m[0]);
    let cls = "";
    if (m[1] !== undefined) cls = "tok-comment";
    else if (m[2] !== undefined) cls = "tok-string";
    else if (m[3] !== undefined) cls = "tok-number";
    else if (m[4] !== undefined) cls = "tok-decorator";
    else if (m[5] !== undefined) {
      if (rules.keywords.has(m[0])) cls = "tok-keyword";
      else if (rules.builtins.has(m[0])) cls = "tok-builtin";
      else if (source[rules.pattern.lastIndex] === "(") cls = "tok-function";
    } else cls = "tok-punct";
    out.push(cls ? `<span class="${cls}">${text}</span>` : text);
    last = rules.pattern.lastIndex;
  }
  if (last < source.length) out.push(escapeHtml(source.slice(last)));
  return out.join("");
}
function updateHighlight() {
  const source = el.editor.value;
  // A trailing newline needs a visible last line, or the mirror is one line
  // shorter than the textarea and scrolling drifts.
  const html = highlightSource(source, languageOf(project.current));
  el.highlightCode.innerHTML = source.endsWith("\n") ? html + " " : html;
}
el.editor.addEventListener("keydown", (event) => {
  if (event.key === "Tab") {
    event.preventDefault();
    const indent = projectLanguage() === "javascript" ? "  " : "    ";
    const { selectionStart, selectionEnd, value } = el.editor;
    el.editor.value = value.slice(0, selectionStart) + indent + value.slice(selectionEnd);
    el.editor.selectionStart = el.editor.selectionEnd = selectionStart + indent.length;
    el.editor.dispatchEvent(new Event("input"));
  } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
    event.preventDefault();
    run();
  } else if (event.key === "Enter") {
    // keep the current line's indentation
    const { selectionStart, value } = el.editor;
    const lineStart = value.lastIndexOf("\n", selectionStart - 1) + 1;
    const indent = /^[ \t]*/.exec(value.slice(lineStart, selectionStart))[0];
    if (indent) {
      event.preventDefault();
      el.editor.setRangeText("\n" + indent, selectionStart, el.editor.selectionEnd, "end");
      el.editor.dispatchEvent(new Event("input"));
    }
  }
});

function validFileName(name) {
  return /^[A-Za-z_][A-Za-z0-9_-]*\.(py|js|mjs)$/.test(name);
}
$("#new-file").addEventListener("click", () => {
  const language = projectLanguage() || "python";
  const name = prompt("New file name:", language === "python" ? "module.py" : "module.js");
  if (!name) return;
  if (!validFileName(name)) { alert("Use a simple name ending in .py or .js (letters, digits, _)."); return; }
  if (project.files.has(name)) { alert(`${name} already exists.`); return; }
  project.files.set(name, "");
  pickEntry();
  renderFiles();
  openFile(name);
});
$("#rename-file").addEventListener("click", () => {
  const from = project.current;
  if (!from) return;
  const to = prompt("Rename file:", from);
  if (!to || to === from) return;
  if (!validFileName(to)) { alert("Use a simple name ending in .py or .js (letters, digits, _)."); return; }
  if (project.files.has(to)) { alert(`${to} already exists.`); return; }
  project.files.set(to, project.files.get(from));
  project.files.delete(from);
  if (project.entry === from) project.entry = to;
  renderFiles();
  openFile(to);
});
$("#delete-file").addEventListener("click", () => {
  const name = project.current;
  if (!name || !confirm(`Delete ${name}?`)) return;
  project.files.delete(name);
  if (project.entry === name) project.entry = null;
  pickEntry();
  renderFiles();
  openFile(project.entry);
});
$("#set-entry").addEventListener("click", () => {
  if (!project.current || !languageOf(project.current)) return;
  project.entry = project.current;
  renderFiles();
  save();
});

// ---- loading folders, files and samples -----------------------------------
async function loadFileObjects(files, folderName) {
  const entries = [];
  const skipped = [];
  for (const file of files) {
    const relative = file.webkitRelativePath || file.name;
    const parts = relative.split("/");
    if (parts.length > 2) { skipped.push(relative); continue; }
    if (!languageOf(file.name)) continue;
    entries.push([file.name, await file.text()]);
  }
  if (entries.length === 0) {
    log("No .py or .js files found at the top level of that selection.", "error");
    return;
  }
  const name = folderName || (files[0].webkitRelativePath || "").split("/")[0] || "files";
  stopRun();
  setProject(name, entries, null, null);
  log(`Opened ${entries.length} file(s) from ${name}.` + (skipped.length ? ` Skipped ${skipped.length} nested file(s); only the top level is used.` : ""), "note");
}
$("#folder-input").addEventListener("change", (event) => {
  loadFileObjects([...event.target.files]);
  event.target.value = "";
});
$("#files-input").addEventListener("change", (event) => {
  loadFileObjects([...event.target.files], "files");
  event.target.value = "";
});

// Drag and drop: a folder (via the File System Entry API) or loose files.
let dragDepth = 0;
document.addEventListener("dragenter", (event) => { event.preventDefault(); dragDepth++; el.dropOverlay.hidden = false; });
document.addEventListener("dragover", (event) => { event.preventDefault(); });
document.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; el.dropOverlay.hidden = true; } });
document.addEventListener("drop", async (event) => {
  event.preventDefault();
  dragDepth = 0;
  el.dropOverlay.hidden = true;
  const items = [...(event.dataTransfer.items || [])];
  const files = [];
  let folderName = null;
  for (const item of items) {
    const entry = item.webkitGetAsEntry ? item.webkitGetAsEntry() : null;
    if (entry && entry.isDirectory) {
      folderName = folderName || entry.name;
      const children = await new Promise((resolve) => entry.createReader().readEntries(resolve, () => resolve([])));
      for (const child of children) {
        if (child.isFile) files.push(await new Promise((resolve) => child.file(resolve, () => resolve(null))));
      }
    } else if (item.kind === "file") {
      files.push(item.getAsFile());
    }
  }
  await loadFileObjects(files.filter(Boolean), folderName);
});

async function loadSample(key) {
  const sample = SAMPLES[key];
  if (!sample) return;
  const entries = [];
  try {
    if (sample.inline) {
      for (const [name, source] of Object.entries(sample.inline)) entries.push([name, source]);
    } else {
      for (const name of sample.files) {
        const response = await fetch(sample.base + name, { cache: "no-store" });
        if (!response.ok) throw new Error(`${response.status} for ${name}`);
        entries.push([name, await response.text()]);
      }
    }
  } catch (error) {
    log(`Could not fetch the sample (${error.message}). Serve the repository root with "node playground/serve.cjs".`, "error");
    return;
  }
  stopRun();
  setProject(sample.name, entries, sample.entry, sample.entry);
  log(`Loaded sample ${sample.name}. Press Run.`, "note");
}
$("#sample-button").addEventListener("click", (event) => {
  event.stopPropagation();
  el.sampleMenu.hidden = !el.sampleMenu.hidden;
});
document.addEventListener("click", () => { el.sampleMenu.hidden = true; });
for (const button of el.sampleMenu.querySelectorAll("button")) {
  button.addEventListener("click", () => { el.sampleMenu.hidden = true; loadSample(button.dataset.sample); });
}

// ---- console + status --------------------------------------------------------
function log(text, kind = "") {
  const line = document.createElement("div");
  if (kind) line.className = kind;
  line.textContent = text;
  el.consoleOut.append(line);
  while (el.consoleOut.childElementCount > MAX_CONSOLE_LINES) el.consoleOut.firstElementChild.remove();
  el.consoleOut.scrollTop = el.consoleOut.scrollHeight;
}
function logConsole(lines) {
  for (const entry of lines || []) log(entry.text, entry.stream === "stdout" ? "" : "stderr");
}
$("#clear-console").addEventListener("click", () => el.consoleOut.replaceChildren());
function setStatus(parts) {
  el.status.replaceChildren();
  for (const [text, cls] of parts) {
    const span = document.createElement("span");
    span.textContent = text;
    if (cls) span.className = cls;
    el.status.append(span);
  }
}

// ---- the engine worker -------------------------------------------------------
const worker = { instance: null, pending: new Map(), nextId: 1, profile: null };
function startWorker() {
  if (worker.instance) worker.instance.terminate();
  for (const p of worker.pending.values()) { clearTimeout(p.timer); p.reject(new Error("engine restarted")); }
  worker.pending.clear();
  worker.instance = new Worker(new URL("./engine.worker.js", import.meta.url), { type: "module" });
  worker.instance.onmessage = (event) => {
    const m = event.data;
    const p = worker.pending.get(m.id);
    if (!p) return;
    clearTimeout(p.timer);
    worker.pending.delete(m.id);
    p.resolve(m);
  };
  worker.instance.onerror = (event) => {
    log(`Engine worker error: ${event.message || event}`, "error");
    setStatus([["Engine failed to load. Build it with ./build-variants.sh all and serve the repository root.", "bad"]]);
  };
  request({ type: "hello" }, 60000).then((reply) => {
    if (reply.type === "error") {
      log(reply.error, "error");
      setStatus([[reply.error, "bad"]]);
      return;
    }
    worker.profile = reply.profile;
    const languages = reply.profile.languages || [];
    if (!languages.includes("python")) {
      log("This engine build has no Python frontend; rebuild with ./build-variants.sh all.", "error");
    }
    refreshStatus();
  }).catch((error) => {
    setStatus([[`Engine did not answer: ${error.message}`, "bad"]]);
  });
}
function request(message, timeoutMs = DEADLINE_MS) {
  return new Promise((resolve, reject) => {
    const id = worker.nextId++;
    const timer = setTimeout(() => {
      worker.pending.delete(id);
      reject(new Error(`no reply within ${timeoutMs / 1000}s`));
    }, timeoutMs);
    worker.pending.set(id, { resolve, reject, timer });
    worker.instance.postMessage({ ...message, id });
  });
}
function refreshStatus() {
  const p = worker.profile;
  const parts = [];
  if (p) parts.push([`zipp-wasm ${p.version} · languages: ${(p.languages || []).join(", ")}`]);
  parts.push([loop.running ? `running ${projectLanguage()} · hooks: ${hookSummary()}` : "idle"]);
  setStatus(parts);
}

// ---- running + the frame loop ---------------------------------------------
const loop = { running: false, busy: false, frames: 0, fpsWindow: [], hooks: {} };
const input = { mx: 0, my: 0, down: false, clicked: false, keys: {}, events: [] };

function hookSummary() {
  const found = Object.entries(loop.hooks).filter(([, v]) => v).map(([k]) => k);
  return found.length ? found.join(", ") : "none";
}
function projectSources(language) {
  const files = {};
  const order = [];
  for (const name of [...project.files.keys()].sort()) {
    if (languageOf(name) !== language) continue;
    const key = language === "python" ? stemOf(name) : name;
    if (name === project.entry) continue;
    files[key] = project.files.get(name);
    order.push(key);
  }
  const entryKey = language === "python" ? stemOf(project.entry) : project.entry;
  files[entryKey] = project.files.get(project.entry);
  order.push(entryKey);
  return { files, order, entry: entryKey };
}
async function run() {
  if (!project.entry) { log("Nothing to run: open a folder or a sample first.", "error"); return; }
  const language = projectLanguage();
  if (!language) { log(`The entry file ${project.entry} is not a .py or .js file.`, "error"); return; }
  stopRun();
  el.consoleOut.replaceChildren();
  clearCanvas();
  const { files, order, entry } = projectSources(language);
  log(`▶ ${project.name} (${language}, entry ${project.entry})`, "note");
  el.run.disabled = true;
  try {
    const reply = await request({ type: "run", language, files, order, entry, budget: INSTRUCTION_BUDGET }, DEADLINE_MS);
    logConsole(reply.console);
    if (reply.type === "error") {
      reportError(reply);
      return;
    }
    render(reply.ui);
    loop.hooks = reply.hooks;
    if (reply.hooks.draw || reply.hooks.update) {
      startLoop();
      log(`Program defines ${hookSummary()}; running frames. Click the canvas for keyboard input.`, "ok");
    } else {
      log("Program finished.", "ok");
      refreshStatus();
    }
  } catch (error) {
    deadline(error);
  } finally {
    el.run.disabled = false;
  }
}
function reportError(reply) {
  log(reply.error, "error");
  if (reply.kind === "resource") log("The engine hit a resource limit and was disposed; press Run to start again.", "note");
  else if (reply.disposed) log("The engine was disposed; press Run to start again.", "note");
  stopLoop();
}
function deadline(error) {
  log(`Stopped: ${error.message}. The program did not respond (an infinite loop?), so its engine was discarded.`, "error");
  stopLoop();
  startWorker();
}
function startLoop() {
  loop.running = true;
  loop.busy = false;
  loop.frames = 0;
  loop.fpsWindow = [];
  input.events = [];
  input.clicked = false;
  el.stop.disabled = false;
  el.canvasHint.hidden = true;
  refreshStatus();
  requestAnimationFrame(tick);
}
function stopLoop() {
  loop.running = false;
  el.stop.disabled = true;
  refreshStatus();
}
async function stopRun() {
  if (!worker.instance) return;
  const wasRunning = loop.running;
  stopLoop();
  try { await request({ type: "stop" }, 2000); } catch { startWorker(); }
  if (wasRunning) log("Stopped.", "note");
}
el.run.addEventListener("click", run);
el.stop.addEventListener("click", stopRun);

async function tick(now) {
  if (!loop.running) return;
  if (loop.busy) { requestAnimationFrame(tick); return; }
  loop.busy = true;
  const events = input.events;
  input.events = [];
  const snapshot = {
    mx: Math.round(input.mx), my: Math.round(input.my), down: input.down, clicked: input.clicked,
    keys: { ...input.keys }, w: el.canvas.width, h: el.canvas.height,
  };
  input.clicked = false;
  try {
    const reply = await request({ type: "frame", input: snapshot, events }, DEADLINE_MS);
    logConsole(reply.console);
    if (reply.type === "error") { render(reply.ui); reportError(reply); return; }
    render(reply.ui);
    loop.frames++;
    loop.fpsWindow.push(now);
    while (loop.fpsWindow.length && now - loop.fpsWindow[0] > 1000) loop.fpsWindow.shift();
    if (loop.frames % 10 === 0) el.frameStats.textContent = `${loop.fpsWindow.length} fps · frame ${loop.frames}`;
  } catch (error) {
    deadline(error);
    return;
  } finally {
    loop.busy = false;
  }
  if (loop.running) requestAnimationFrame(tick);
}

// ---- canvas rendering + input ---------------------------------------------
const paint = { font: 14 };
function clearCanvas() {
  ctx.fillStyle = "#10141c";
  ctx.fillRect(0, 0, el.canvas.width, el.canvas.height);
  el.frameStats.textContent = "";
}
function applyFont() {
  ctx.font = `${paint.font}px system-ui, -apple-system, "Segoe UI", sans-serif`;
  ctx.textBaseline = "alphabetic";
}
function render(commands) {
  if (!Array.isArray(commands) || commands.length === 0) return;
  el.canvasHint.hidden = true;
  applyFont();
  for (const c of commands) {
    switch (c[0]) {
      case "canvas": {
        const w = Math.max(1, Math.min(4096, c[1] | 0)), h = Math.max(1, Math.min(4096, c[2] | 0));
        if (el.canvas.width !== w || el.canvas.height !== h) { el.canvas.width = w; el.canvas.height = h; applyFont(); }
        break;
      }
      case "clear": ctx.fillStyle = c[1]; ctx.fillRect(0, 0, el.canvas.width, el.canvas.height); break;
      case "rect": ctx.fillStyle = c[5]; ctx.fillRect(c[1], c[2], c[3], c[4]); break;
      case "circle": ctx.fillStyle = c[4]; ctx.beginPath(); ctx.arc(c[1], c[2], Math.max(0, c[3]), 0, Math.PI * 2); ctx.fill(); break;
      case "line": ctx.strokeStyle = c[5]; ctx.lineWidth = 2; ctx.beginPath(); ctx.moveTo(c[1], c[2]); ctx.lineTo(c[3], c[4]); ctx.stroke(); break;
      case "text": ctx.fillStyle = c[4]; ctx.fillText(c[3], c[1], c[2]); break;
      case "font": paint.font = Math.max(4, Math.min(200, c[1])); applyFont(); break;
      case "button": {
        const [, x, y, w, h, label] = c;
        const hover = input.mx >= x && input.mx < x + w && input.my >= y && input.my < y + h;
        ctx.fillStyle = hover ? (input.down ? "#1f6feb" : "#30363d") : "#21262d";
        ctx.strokeStyle = "#8b949e";
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.roundRect(x + 0.5, y + 0.5, w - 1, h - 1, 5);
        ctx.fill();
        ctx.stroke();
        ctx.fillStyle = "#e6edf3";
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText(label, x + w / 2, y + h / 2);
        ctx.textAlign = "start";
        ctx.textBaseline = "alphabetic";
        break;
      }
      default: break;
    }
  }
}
function canvasPoint(event) {
  const rect = el.canvas.getBoundingClientRect();
  return {
    x: (event.clientX - rect.left) * (el.canvas.width / rect.width),
    y: (event.clientY - rect.top) * (el.canvas.height / rect.height),
  };
}
el.canvas.addEventListener("mousemove", (event) => { const p = canvasPoint(event); input.mx = p.x; input.my = p.y; });
el.canvas.addEventListener("mousedown", (event) => { const p = canvasPoint(event); input.mx = p.x; input.my = p.y; input.down = true; el.canvas.focus(); });
window.addEventListener("mouseup", () => { input.down = false; });
el.canvas.addEventListener("click", (event) => {
  const p = canvasPoint(event);
  input.clicked = true;
  input.events.push({ type: "click", x: Math.round(p.x), y: Math.round(p.y) });
});
el.canvas.addEventListener("keydown", (event) => {
  if (event.key === "Tab") return;
  event.preventDefault();
  if (!input.keys[event.key]) input.events.push({ type: "key", key: event.key });
  input.keys[event.key] = true;
});
el.canvas.addEventListener("keyup", (event) => { delete input.keys[event.key]; });
el.canvas.addEventListener("blur", () => { input.keys = {}; });

// ---- boot ---------------------------------------------------------------------
startWorker();
clearCanvas();
if (!restore()) loadSample("python");
else log(`Restored ${project.name} from this browser. Press Run.`, "note");
renderFiles();
