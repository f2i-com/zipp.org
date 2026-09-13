"""Real-origin GPU acceptance. Requires Python Playwright and installed Chrome.

Run from the repository root: python crates/zipp-wasm/playground/nca-smoke.py
Does not accept missing hardware as a passed GPU test. Artifacts: target/nca-smoke.
"""
import json
import os
from pathlib import Path
import subprocess
import socket
import time
import urllib.request
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / 'target/nca-smoke'
# Reserve an ephemeral port briefly; startup below verifies that our own server
# acquired it before sending any run/stop request.
with socket.socket() as reservation:
    reservation.bind(('127.0.0.1', 0))
    PORT = reservation.getsockname()[1]
BASE = f'http://127.0.0.1:{PORT}'


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    with (OUT / 'server.log').open('w') as log:
        server = subprocess.Popen(['node', str(Path(__file__).with_name('serve.cjs'))], cwd=ROOT,
                                  env=dict(os.environ, PORT=str(PORT)), stdout=log, stderr=log)
        owns_server = False
        try:
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError('Test server failed to start; inspect target/nca-smoke/server.log')
                if f'native GPU lab: {BASE}/' in (OUT / 'server.log').read_text():
                    owns_server = True
                    break
                time.sleep(.1)
            if not owns_server:
                raise RuntimeError('Timed out waiting for the owned test server')
            with sync_playwright() as p:
                browser = p.chromium.launch(channel=os.environ.get('PLAYWRIGHT_CHANNEL', 'chrome'), headless=True)
                page = browser.new_page(viewport=dict(width=1500, height=1180))
                errors = []
                page.on('pageerror', lambda err: errors.append(str(err)))
                page.goto(BASE + '/crates/zipp-wasm/playground/nca.html')
                page.wait_for_function("!document.querySelector('#run').disabled", timeout=75000)
                assert 'hardware WebGL2' in page.locator('#renderer').inner_text(), page.locator('#renderer').inner_text()
                status = page.request.get(BASE + '/api/nca/status').json()
                assert not status.get('error'), status
                devices = [d['id'] for d in status['devices'] if d['usable']]
                assert devices, 'No working CUDA device; GPU acceptance cannot pass'
                mapped_devices = [device for gpu in status['gpus'] for device in gpu['cudaDevices']]
                assert set(devices).issubset(mapped_devices), 'GPU telemetry UUID mapping is incomplete'
                print('CUDA devices:', devices, flush=True)
                headers = {'content-type': 'application/json', 'x-nca-token': status['token']}
                # Local bridge contract checks use only the fixed API.
                assert page.request.post(BASE + '/api/nca/run', data={'mode': 'memory', 'devices': devices}).status == 403
                assert page.request.post(BASE + '/api/nca/run', headers=headers, data={'mode': 'other', 'devices': devices}).status == 400
                assert page.request.post(BASE + '/api/nca/run', headers=headers, data={'mode': 'memory', 'devices': ['cuda:9999']}).status == 400
                for d in devices:
                    page.locator(f'#devices input[value="{d}"]').check()
                page.locator('#run').click()
                page.wait_for_function("document.querySelector('#connection').textContent.includes('complete') && document.querySelector('#stop').disabled", timeout=75000)
                status = page.request.get(BASE + '/api/nca/status').json()
                assert len(status['jobs']) == len(devices)
                for job in status['jobs']:
                    assert job['state'] == 'complete', job
                    assert job['result']['sharedWeightsUnchanged'], job
                    assert job['result']['observed'] == job['result']['answer'], job
                assert page.locator('#empty').is_hidden()
                page.screenshot(path=str(OUT / 'memory.png'), full_page=True)
                print('Memory replay passed on all selected GPUs', flush=True)
                # Verify real GPU shaders against the reference on this origin.
                reports = page.evaluate("""async () => {
                  const {checkBackend} = await import('../gpu-lab/tests/browser-cases.mjs');
                  return [await checkBackend('webgl2'), await checkBackend('webgpu')];
                }""")
                for report in reports:
                    assert report['status'] == 'passed' and report['passed'] == 15, report
                    print(report['backend'], report['passed'], report['info']['adapter'], flush=True)
                (OUT / 'backends.json').write_text(json.dumps(reports, indent=2))

                def run_mode(mode, steps=4, count=8):
                    page.locator('#mode').select_option(mode)
                    if mode.endswith('-train'):
                        page.locator('#steps').fill(str(steps)); page.locator('#batch').fill('4')
                    if mode.startswith('language'):
                        page.locator('#bytes').fill(str(count))
                    page.locator('#run').click()
                    page.wait_for_function("document.querySelector('#run').disabled")
                    page.wait_for_function("!document.querySelector('#run').disabled", timeout=90000)
                    s = page.request.get(BASE + '/api/nca/status').json()
                    assert all(j['state'] == 'complete' for j in s['jobs']), s['jobs']
                    return s

                run_mode('memory-train')
                s = run_mode('language-train')
                assert page.locator('#loss-line').get_attribute('points'), 'Missing loss curve'
                page.screenshot(path=str(OUT / 'language-training.png'), full_page=True)
                run_mode('language', count=16)
                page.locator('#prompt').fill('--été 🧪')
                unicode_run = run_mode('language', count=2)
                assert all(j['result']['text'].startswith('--été 🧪') for j in unicode_run['jobs'])
                page.locator('#prompt').fill('The ')
                print('Memory training, language training, checkpoint generation passed', flush=True)
                # CPU remains an explicit choice and never masquerades as CUDA.
                for d in devices:
                    page.locator(f'#devices input[value="{d}"]').uncheck()
                page.locator('#devices input[value="cpu"]').check()
                s = run_mode('memory')
                assert s['jobs'][0]['result']['device'] == 'cpu'
                page.locator('#devices input[value="cpu"]').uncheck()
                page.locator(f'#devices input[value="{devices[0]}"]').check()
                page.locator('#mode').select_option('language-train')
                page.locator('#steps').fill('5000')
                page.locator('#frame-status').evaluate("element => element.textContent = 'waiting for new training frame'")
                page.locator('#run').click()
                page.wait_for_function("/training [0-9]+/.test(document.querySelector('#frame-status').textContent)", timeout=60000)
                page.locator('#stop').click()
                page.wait_for_function("!document.querySelector('#run').disabled", timeout=20000)
                stopped = page.request.get(BASE + '/api/nca/status').json()
                assert not stopped['running'] and stopped['jobs'][0]['state'] == 'stopped', stopped['jobs']
                page.reload()
                page.wait_for_function("!document.querySelector('#run').disabled", timeout=20000)
                assert 'stopped' in page.locator('#connection').inner_text()
                assert not errors, errors
                print('CPU, stop, reload, API validation and browser error checks passed', flush=True)
                page.set_viewport_size(dict(width=390, height=844))
                assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), 'Mobile horizontal overflow'
                page.screenshot(path=str(OUT / 'mobile.png'), full_page=True)
                # Rendering failures must not prevent starting/stopping native work.
                page.evaluate("document.querySelector('#state').getContext('webgl2').getExtension('WEBGL_lose_context').loseContext()")
                page.wait_for_function("document.querySelector('#state').getContext('webgl2').isContextLost()")
                run_mode('memory')
                assert 'WebGL context lost' in page.locator('#renderer').inner_text()
                assert page.locator('#stop').is_disabled()
                assert not errors, errors
                print('Unicode prompts, device UUIDs and lost-WebGL-context handling passed', flush=True)
                browser.close()
        finally:
            # Stop any fixed child workloads before terminating this test server.
            try:
                if owns_server and server.poll() is None:
                    s = json.load(urllib.request.urlopen(BASE + '/api/nca/status', timeout=3))
                    req = urllib.request.Request(BASE + '/api/nca/stop', data=b'{}', headers={'Content-Type': 'application/json', 'X-Nca-Token': s['token']})
                    urllib.request.urlopen(req, timeout=3).close()
            except OSError:
                pass
            server.terminate(); server.wait(timeout=10)


if __name__ == '__main__':
    main()
