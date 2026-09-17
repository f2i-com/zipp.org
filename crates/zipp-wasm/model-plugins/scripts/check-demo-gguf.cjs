// Drive the model-plugin lab in a real browser against a real GGUF file.
const {chromium} = require('playwright');

const ORIGIN = 'http://127.0.0.1:8767';
const PAGE = `${ORIGIN}/crates/zipp-wasm/model-plugins/demo/`;
const MODEL = process.env.MODEL || 'E:/models/qwen3-0.6b-q4_k_m.gguf';

(async () => {
  const browser = await chromium.launch({
    channel: process.env.PLAYWRIGHT_CHANNEL || 'chrome',
    args: ['--enable-unsafe-webgpu', '--ignore-gpu-blocklist'],
  });
  const page = await browser.newPage();
  page.on('dialog', d => d.accept());
  page.on('console', m => { if (m.type() === 'error') console.log('  [console]', m.text().slice(0, 160)); });
  await page.goto(PAGE, {waitUntil: 'domcontentloaded'});

  // 1. The optional module is detected and reported, without being asked to run.
  await page.selectOption('#source', 'gguf');
  await page.waitForFunction(
    () => !/Checking/.test(document.getElementById('gguf-support').textContent), null, {timeout: 20000});
  const support = await page.textContent('#gguf-support');
  console.log('  support line:', support.trim());

  // 2. Load an actual checkpoint and generate.
  await page.setInputFiles('#gguf', MODEL);
  await page.fill('#prompt', process.env.PROMPT || 'The capital of France is');
  await page.fill('#count', process.env.COUNT || '8');
  await page.selectOption('#backend', process.env.BACKEND || 'wasm');
  await page.click('#run');

  const started = Date.now();
  try {
    await page.waitForFunction(
      () => /Finished|Stopped|^[A-Z]+:/.test(document.getElementById('status').textContent.trim()),
      null, {timeout: 300000});
  } catch { /* fall through to report whatever the page shows */ }
  const status = (await page.textContent('#status')).trim();
  const output = (await page.textContent('#output')).trim();
  const details = (await page.textContent('#details')).trim();
  console.log(`  status : ${status}  (${((Date.now() - started) / 1000).toFixed(1)} s)`);
  console.log(`  output : ${JSON.stringify(output)}`);
  const info = details.split('\n\n')[0];
  try {
    const parsed = JSON.parse(info);
    console.log(`  model  : ${parsed.name} (${parsed.architecture}), ${parsed.residentMB} MB resident, ` +
                `${parsed.quantizedMB} MB of blocks, float32 would be ${parsed.asFloat32MB} MB`);
    console.log(`  tokenizer: ${parsed.tokenizer.pre}, ${parsed.tokenizer.vocab} entries`);
  } catch { console.log('  details:', info.slice(0, 200)); }

  await browser.close();
  const ok = /Finished/.test(status) && output.length > 0;
  console.log(ok ? '\n  ok   the lab loaded a GGUF checkpoint and generated from it'
                 : '\n  FAIL the run did not finish');
  process.exit(ok ? 0 : 1);
})();
