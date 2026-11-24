import { mountSoftNContainer } from '/container-demo/runtime/softn.js';

/** Minimal browser-native ZIP reader (Stored + Deflate via DecompressionStream) for the sandbox inspector. */
async function openSoftNZip(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let eocd = -1;
  for (let i = bytes.byteLength - 22; i >= Math.max(0, bytes.byteLength - 65557); i--) {
    if (view.getUint32(i, true) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  const index = new Map();
  if (eocd >= 0) {
    const cdSize = view.getUint32(eocd + 12, true);
    const cdOffset = view.getUint32(eocd + 16, true);
    const dec = new TextDecoder();
    let pos = cdOffset;
    const end = Math.min(bytes.byteLength, cdOffset + cdSize);
    while (pos + 46 <= end && view.getUint32(pos, true) === 0x02014b50) {
      const method = view.getUint16(pos + 10, true);
      const compSize = view.getUint32(pos + 20, true);
      const nameLen = view.getUint16(pos + 28, true);
      const extraLen = view.getUint16(pos + 30, true);
      const commentLen = view.getUint16(pos + 32, true);
      const localOffset = view.getUint32(pos + 42, true);
      const name = dec.decode(bytes.subarray(pos + 46, pos + 46 + nameLen));
      index.set(name, { method, compSize, localOffset });
      pos += 46 + nameLen + extraLen + commentLen;
    }
  }
  const cache = new Map();
  async function readText(name) {
    if (cache.has(name)) return cache.get(name);
    const meta = index.get(name);
    if (!meta) return null;
    if (view.getUint32(meta.localOffset, true) !== 0x04034b50) return null;
    const lNameLen = view.getUint16(meta.localOffset + 26, true);
    const lExtraLen = view.getUint16(meta.localOffset + 28, true);
    const dataStart = meta.localOffset + 30 + lNameLen + lExtraLen;
    const slice = bytes.subarray(dataStart, dataStart + meta.compSize);
    let raw = slice;
    if (meta.method === 8 && typeof DecompressionStream !== 'undefined') {
      const ds = new DecompressionStream('deflate-raw');
      const stream = new Blob([slice]).stream().pipeThrough(ds);
      raw = new Uint8Array(await new Response(stream).arrayBuffer());
    } else if (meta.method !== 0) {
      return null;
    }
    const text = new TextDecoder().decode(raw);
    cache.set(name, text);
    return text;
  }
  return { readText };
}

const APPS = {
  pottery: {
    id: 'pottery',
    name: 'Kiln & Wheel Pottery Studio',
    bundleUrl: '/container-demo/bundles/pottery-studio.softn',
    frontendTier: 'zipp-javascript-lite',
    backendFile: 'server/index.php',
    schemaFile: 'server/schema.sql',
    blurb: 'FormLogic live homepage SoftN container (9.9 KB): standard HTML/JS UI + PHP 8 PDO backend + SQLite 3 schema running completely in browser WebAssembly Workers.',
  },
  brewdesk: {
    id: 'brewdesk',
    name: 'BrewDesk Café Ops',
    bundleUrl: '/container-demo/bundles/brewdesk.softn',
    frontendTier: 'zipp-javascript',
    backendFile: 'server/public/index.php',
    schemaFile: 'server/migrations/schema.sql',
    blurb: 'FormLogic Marketplace business pack (242 KB): multi-view café & barista ops desk backed by Slim 4 + PHP 8 + SQLite 3 inside the exact same ZIPP WASM container.',
  },
};

const $ = (id) => document.getElementById(id);
const bootOverlay = $('boot-overlay');
const bootTitle = $('boot-title');
const bootDetail = $('boot-detail');
const bootProgress = $('boot-progress');
const statusPill = $('status-pill');
const telFe = $('tel-fe');
const telBe = $('tel-be');
const inspector = $('inspector');
const inspectBtn = $('inspect-btn');
const resetBtn = $('reset-btn');
const inspectorNote = $('inspector-note');
const inspectorCode = $('inspector-code');

let currentAppId = 'pottery';
let currentHandle = null;
let currentArchive = null;
let currentTab = 'manifest';
let mountSeq = 0;
const logs = [];

function addLog(line) {
  const ts = new Date().toISOString().slice(11, 23);
  logs.push(`[${ts}] ${line}`);
  if (logs.length > 200) logs.shift();
  if (!inspector.hidden && currentTab === 'boundary') renderInspector();
}

function setStatus(state, text) {
  statusPill.dataset.state = state;
  statusPill.textContent = text;
}

async function renderInspector() {
  const spec = APPS[currentAppId];
  if (!spec) return;
  if (currentTab === 'boundary') {
    inspectorNote.textContent = 'Live isolation trace from the SoftN v2 container host and ZIPP v0.0.40 WebAssembly Workers.';
    inspectorCode.textContent = [
      `Container: ${spec.name} (${spec.bundleUrl})`,
      `Frontend Worker: ZIPP v0.0.40 (${telFe.textContent}) -> Closed ShadowRoot`,
      `Backend Worker:  ZIPP v0.0.40 (zipp-php PhpApp SAPI) -> XDB SQLite 3 WASM`,
      `Permissions:     default-deny (0 host permissions granted; no external egress)`,
      `Host Isolation:  guest scripts cannot reach window, document.cookie, or host DOM`,
      '',
      '--- Live event trace ---',
      ...logs,
    ].join('\n');
    return;
  }
  if (!currentArchive) {
    inspectorNote.textContent = 'Loading archive…';
    inspectorCode.textContent = '';
    return;
  }
  const tab = currentTab;
  const appAtStart = currentAppId;
  if (tab === 'manifest') {
    inspectorNote.textContent = `${spec.blurb} Below is manifest.json inside the .softn archive.`;
    const text = (await currentArchive.readText('manifest.json')) || '(no manifest.json)';
    if (currentTab === tab && currentAppId === appAtStart) inspectorCode.textContent = text;
  } else if (tab === 'backend') {
    inspectorNote.textContent = `${spec.backendFile} runs inside an isolated ZIPP v0.0.40 PHP 8 Worker; PDO('sqlite:app.db') routes through addAppBridge('sql', ...) to XDB SQLite WASM.`;
    const src = (await currentArchive.readText(spec.backendFile)) || '(backend entry not found)';
    if (currentTab === tab && currentAppId === appAtStart) {
      inspectorCode.textContent = src.length > 16000 ? `${src.slice(0, 16000)}\n\n// … (${src.length.toLocaleString('en-US')} bytes total)` : src;
    }
  } else if (tab === 'schema') {
    inspectorNote.textContent = `${spec.schemaFile} seeds the app's private SQLite 3 database in browser memory (ephemeral per demo session).`;
    const src = (await currentArchive.readText(spec.schemaFile)) || '(schema not found)';
    if (currentTab === tab && currentAppId === appAtStart) {
      inspectorCode.textContent = src.length > 16000 ? `${src.slice(0, 16000)}\n\n-- … (${src.length.toLocaleString('en-US')} bytes total)` : src;
    }
  }
}

async function mountDemo(appId) {
  const spec = APPS[appId] || APPS.pottery;
  currentAppId = spec.id;
  const seq = ++mountSeq;
  logs.length = 0;

  for (const btn of document.querySelectorAll('.app-switch button')) {
    const active = btn.dataset.app === spec.id;
    btn.classList.toggle('active', active);
    btn.setAttribute('aria-selected', String(active));
  }

  telFe.textContent = spec.frontendTier;
  telBe.textContent = 'zipp-php';
  setStatus('booting', 'Starting container…');
  resetBtn.disabled = true;
  bootOverlay.hidden = false;
  bootTitle.textContent = `Booting ${spec.name}…`;
  bootDetail.textContent = 'Fetching .softn container archive and initializing ZIPP v0.0.40 Workers.';
  bootProgress.style.width = '12%';

  if (currentHandle) {
    try { currentHandle.dispose(); } catch { /* ignore */ }
    currentHandle = null;
  }

  const prevStage = document.getElementById('container-stage');
  const freshStage = document.createElement('div');
  freshStage.id = 'container-stage';
  freshStage.className = 'container-stage';
  prevStage.replaceWith(freshStage);

  try {
    addLog(`Fetching ${spec.bundleUrl}`);
    const response = await fetch(spec.bundleUrl, { cache: 'no-cache' });
    if (!response.ok) throw new Error(`HTTP ${response.status} loading ${spec.bundleUrl}`);
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (seq !== mountSeq) return;
    addLog(`Archive loaded (${bytes.byteLength.toLocaleString('en-US')} bytes)`);
    currentArchive = await openSoftNZip(bytes);
    if (seq !== mountSeq) return;
    void renderInspector();

    const handle = await mountSoftNContainer(freshStage, {
      source: bytes,
      runtimeUrl: '/container-demo/runtime/',
      // Reuse the site's v0.0.40 full JavaScript WASM from /sandbox/ without duplicating 5 MB.
      engineWasm: {
        javascript: '/sandbox/zipp-0.0.40-javascript.wasm',
      },
      appId: `zipp-demo-${spec.id}-${Date.now()}-${seq}`,
      persistStorage: false,
      permissionMode: 'deny',
      downloads: 'deny',
      limits: {
        instructionsPerTurn: 150_000_000,
        turnTimeoutMs: 20_000,
      },
      onStatus(status) {
        if (seq !== mountSeq) return;
        const pct = typeof status.percent === 'number' ? Math.max(10, Math.min(98, status.percent)) : 45;
        bootProgress.style.width = `${pct}%`;
        bootTitle.textContent = status.message || `Booting ${spec.name}…`;
        if (status.detail) bootDetail.textContent = status.detail;
        addLog(`[${status.phase}] ${status.message}${status.detail ? ` (${status.detail})` : ''}`);
      },
      onConsole(level, text) {
        if (seq !== mountSeq) return;
        addLog(`console.${level}: ${text}`);
      },
      onReady(info) {
        if (seq !== mountSeq) return;
        telFe.textContent = info.engineTier === 'lite' ? 'zipp-javascript-lite' : `zipp-${info.engineTier}`;
        bootProgress.style.width = '100%';
        bootOverlay.hidden = true;
        setStatus('ready', `Running · ${info.engineTier} + php`);
        resetBtn.disabled = false;
        addLog(`Container ready (tier=${info.engineTier}, permissions=${info.capabilitiesRequested.length ? info.capabilitiesRequested.join(',') : 'none'})`);
      },
      onError(err) {
        if (seq !== mountSeq) return;
        addLog(`ERROR: ${err?.message || String(err)}`);
        setStatus('error', 'Container error');
        bootTitle.textContent = 'Container failed to start';
        bootDetail.textContent = err?.message || String(err);
        bootOverlay.hidden = false;
        resetBtn.disabled = false;
      },
    });
    if (seq !== mountSeq) {
      handle.dispose();
      return;
    }
    currentHandle = handle;
  } catch (err) {
    if (seq !== mountSeq) return;
    addLog(`BOOT ERROR: ${err?.message || String(err)}`);
    setStatus('error', 'Boot failed');
    bootTitle.textContent = 'Could not boot container';
    bootDetail.textContent = err?.message || String(err);
    bootOverlay.hidden = false;
    resetBtn.disabled = false;
  }
}

for (const btn of document.querySelectorAll('.app-switch button')) {
  btn.addEventListener('click', () => {
    if (btn.dataset.app && btn.dataset.app !== currentAppId) {
      void mountDemo(btn.dataset.app);
    }
  });
}

inspectBtn.addEventListener('click', () => {
  const open = inspector.hidden;
  inspector.hidden = !open;
  inspectBtn.setAttribute('aria-expanded', String(open));
  if (open) renderInspector();
});

$('close-inspector').addEventListener('click', () => {
  inspector.hidden = true;
  inspectBtn.setAttribute('aria-expanded', 'false');
});

for (const tab of document.querySelectorAll('.inspector-tabs button')) {
  tab.addEventListener('click', () => {
    currentTab = tab.dataset.file || 'manifest';
    for (const b of document.querySelectorAll('.inspector-tabs button')) {
      const active = b === tab;
      b.classList.toggle('active', active);
      b.setAttribute('aria-selected', String(active));
    }
    renderInspector();
  });
}

resetBtn.addEventListener('click', () => {
  void mountDemo(currentAppId);
});

const params = new URLSearchParams(location.search);
const initialApp = params.get('app') === 'brewdesk' ? 'brewdesk' : 'pottery';
void mountDemo(initialApp);
