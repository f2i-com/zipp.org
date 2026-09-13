"""Capture actual local runs for the README and landing page.

Requires Python Playwright, installed Chrome, ffmpeg on PATH, the Python-enabled
WASM build and hardware WebGL2. No synthetic model frames.
Run from repository root: python crates/zipp-wasm/playground/capture-demos.py
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import urllib.request
from datetime import datetime, timezone
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / 'landing/public/demos'


def capture(page, name, frames, seconds=10, fps=6):
    folder = frames / name
    folder.mkdir()
    count = round(seconds * fps)
    start = time.perf_counter()
    for i in range(count):
        remaining = start + i / fps - time.perf_counter()
        if remaining > 0:
            time.sleep(remaining)
        page.screenshot(path=str(folder / f'{i:04}.png'))
    elapsed = time.perf_counter() - start
    rate = count / elapsed
    ffmpeg = shutil.which('ffmpeg')
    palette = folder / 'palette.png'
    common = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y', '-framerate', str(rate), '-i', str(folder / '%04d.png')]
    subprocess.run(common + ['-vf', 'scale=1120:-1:flags=lanczos,palettegen=stats_mode=diff', '-frames:v', '1', '-update', '1', str(palette)], check=True)
    subprocess.run(common + ['-i', str(palette), '-lavfi', 'scale=1120:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle', '-loop', '0', str(OUT / f'{name}.gif')], check=True)
    # A separate static poster supports readers who prefer reduced motion.
    shutil.copyfile(folder / f'{count - 1:04}.png', OUT / f'{name}.png')
    print(f'{name}: {count} real frames over {elapsed:.1f}s, {(OUT / (name + ".gif")).stat().st_size / 2**20:.2f} MiB', flush=True)
    return dict(frames=count, capturedSeconds=round(elapsed, 2), playbackFps=round(rate, 3))


def main():
    if not shutil.which('ffmpeg'):
        raise RuntimeError('ffmpeg must be on PATH')
    OUT.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    frames = ROOT / 'target/demo-capture' / stamp
    frames.mkdir(parents=True)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    owned = False
    with (frames / 'server.log').open('w') as log:
        server = subprocess.Popen(['node', str(Path(__file__).with_name('serve.cjs'))], cwd=ROOT,
                                  env=dict(os.environ, PORT=str(port)), stdout=log, stderr=log)
        try:
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError('Capture server failed to start')
                if f'zipp playground: {base}/' in (frames / 'server.log').read_text():
                    owned = True
                    break
                time.sleep(.1)
            if not owned:
                raise RuntimeError('Capture server did not become ready')
            with sync_playwright() as p:
                browser = p.chromium.launch(channel=os.environ.get('PLAYWRIGHT_CHANNEL', 'chrome'), headless=True)
                page = browser.new_page(viewport=dict(width=1400, height=960), device_scale_factor=1)
                errors = []
                page.on('pageerror', lambda e: errors.append(str(e)))
                page.goto(base + '/crates/zipp-wasm/playground/')
                page.wait_for_function("document.querySelector('#status').textContent.includes('languages:')", timeout=60000)
                profile = page.locator('#status').inner_text()
                assert 'python' in profile.lower(), profile
                page.locator('#sample-button').click()
                page.locator('[data-sample="gpu"]').click()
                page.wait_for_function("document.querySelector('#project-name').textContent === 'gpu-compute'")
                page.locator('#gpu-backend').select_option('webgl2')
                page.locator('#run').click()
                page.wait_for_function("document.querySelector('#console').textContent.includes('GPU compute: webgl2')", timeout=20000)
                page.wait_for_function("document.querySelector('#frame-stats').textContent.length > 0", timeout=15000)
                gpu_log = page.locator('#console').inner_text()
                assert 'NVIDIA' in gpu_log, gpu_log
                # This is a real UI screenshot with the Python editor, canvas,
                # selected backend, device log and running WASM status visible.
                page.screenshot(path=str(OUT / 'python-playground.png'))
                clips = {'python-life': capture(page, 'python-life', frames)}
                page.locator('#stop').click()

                if errors:
                    raise RuntimeError(str(errors))
                tracked_sources = ['examples/python/gpu/main.py', 'crates/zipp-wasm/playground/engine.worker.js']
                provenance = dict(capturedAtUtc=stamp, browser=browser.version, wasmProfile=profile,
                    backend=gpu_log, clips=clips,
                    sourceSha256={name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in tracked_sources},
                    note='Actual local browser captures. Python Life runs in Zipp WASM and submits WebGL2 graphs. GIFs are demonstrations, not benchmarks.')
                (OUT / 'provenance.json').write_text(json.dumps(provenance, indent=2), encoding='utf-8')
                browser.close()
        finally:
            server.terminate()
            server.wait(timeout=10)


if __name__ == '__main__':
    main()
