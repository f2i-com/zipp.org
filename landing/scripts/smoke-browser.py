"""Exercise the published landing/playground against a running preview.

Requires Python Playwright and Chrome. Start `npm run build` / `npm run preview`,
then run: python scripts/smoke-browser.py http://127.0.0.1:5190
Set REQUIRE_GPU=1 to require both hardware WebGL2 and WebGPU on a GPU test host.
"""
import os
import re
from pathlib import Path
import sys
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'target/landing-smoke'
BASE = (sys.argv[1] if len(sys.argv) > 1 else 'http://127.0.0.1:5190').rstrip('/')


def sample(page, name, project):
    page.locator('#sample-button').click()
    page.locator(f'[data-sample="{name}"]').click()
    expect(page.locator('#project-name')).to_have_text(project)


def run(page, expected):
    page.locator('#run').click()
    expect(page.locator('#console')).to_contain_text(expected, timeout=30000)


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    OUT.mkdir(parents=True, exist_ok=True)
    fixture = OUT / 'uploaded-project'
    (fixture / 'data').mkdir(parents=True, exist_ok=True)
    (fixture / 'main.py').write_text('import sys\nimport helper\nimport json\nprint("folder-result", helper.double(json.load(open("data/config.json"))["value"]), sys.argv[1])\n', encoding='utf-8')
    (fixture / 'helper.py').write_text('def double(n):\n    return n * 2\n', encoding='utf-8')
    (fixture / 'data/config.json').write_text('{"value":21}', encoding='utf-8')
    with sync_playwright() as p:
        browser = p.chromium.launch(channel=os.environ.get('PLAYWRIGHT_CHANNEL', 'chrome'), headless=True)
        context = browser.new_context(viewport=dict(width=1440, height=1000), reduced_motion='reduce')
        page = context.new_page()
        errors, failed, posts = [], [], []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('response', lambda response: failed.append((response.status, response.url)) if response.status >= 400 else None)
        page.on('request', lambda request: posts.append(request.url) if request.method == 'POST' else None)
        page.goto(BASE + '/')
        page.locator('#recorded-demos').scroll_into_view_if_needed()
        page.get_by_role('button', name='Play animations', exact=True).wait_for()
        assert page.locator('.recorded-card img[src$=".png"]').count() == 3
        page.locator('#recorded-demos').screenshot(path=str(OUT / 'recorded-demos.png'))
        page.get_by_role('button', name='Play animations', exact=True).click()
        assert page.locator('.recorded-card img[src$=".gif"]').count() == 3
        page.get_by_role('button', name='Pause animations', exact=True).click()
        page.emulate_media(reduced_motion='no-preference')
        expect(page.get_by_role('button', name='Pause animations', exact=True)).to_be_visible()
        assert page.locator('.recorded-card img[src$=".gif"]').count() == 3
        page.locator('#playground').scroll_into_view_if_needed()
        frame = page.locator('iframe.project-playground-frame').content_frame
        frame.locator('#status').filter(has_text='languages: javascript, python').wait_for(timeout=60000)
        frame.locator('#sample-button').click()
        frame.locator('[data-sample="python-hello"]').click()
        frame.locator('#project-name').filter(has_text='python-hello').wait_for()
        frame.locator('#run').click()
        frame.locator('#console').filter(has_text='fib(30) = 832040').wait_for(timeout=30000)
        page.locator('#playground').screenshot(path=str(OUT / 'embedded-playground.png'))
        page.locator('.classic-examples > summary').click()
        page.get_by_role('button', name='Run with ZIPP').click()
        expect(page.get_by_label('Zipp console output')).to_contain_text('SAVE GAME', timeout=30000)
        page.locator('.classic-examples > summary').click()
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), 'desktop overflow'
        page.set_viewport_size(dict(width=390, height=844))
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), 'mobile landing overflow'
        page.locator('#playground').screenshot(path=str(OUT / 'embedded-mobile.png'))
        print('Landing media controls, embedded Python, desktop/mobile layout passed.', flush=True)

        page.goto(BASE + '/playground')
        expect(page.locator('#status')).to_contain_text('languages:', timeout=60000)
        assert page.url.endswith('/playground/'), page.url
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), 'mobile playground overflow'
        page.set_viewport_size(dict(width=1400, height=960))
        sample(page, 'javascript-hello', 'js-hello')
        run(page, 'fib(20) = 6765')
        for name, project in [('python', 'python-balls'), ('javascript', 'js-balls'), ('ant', 'langtons-ant')]:
            sample(page, name, project)
            run(page, 'running frames')
            expect(page.locator('#frame-stats')).not_to_be_empty(timeout=30000)
            assert not page.locator('#console .error').count(), page.locator('#console').inner_text()
            page.locator('#stop').click()
        page.locator('#folder-input').set_input_files(str(fixture))
        page.locator('#project-name').filter(has_text='uploaded-project').wait_for()
        page.locator('#program-args').fill('uploaded')
        run(page, 'folder-result 42 uploaded')
        assert 'config.json' in page.locator('#file-list').inner_text()
        page.reload()
        expect(page.locator('#status')).to_contain_text('languages:', timeout=60000)
        page.locator('#program-args').fill('uploaded')
        run(page, 'folder-result 42 uploaded')
        assert not posts, f'Unexpected upload requests: {posts}'
        print('Clean route, JavaScript, nested Python folder/data/imports/arguments and reload passed.', flush=True)

        sample(page, 'gpu', 'gpu-compute')
        backends = ['wasm', 'webgl2', 'webgpu'] if os.environ.get('REQUIRE_GPU') == '1' else ['wasm']
        for backend in backends:
            page.locator('#gpu-backend').select_option(backend)
            run(page, f'GPU compute: {backend}')
            expect(page.locator('#console')).to_contain_text(re.compile(r'\[58(?:\.0)?, 64(?:\.0)?, 139(?:\.0)?, 154(?:\.0)?\]'), timeout=30000)
            expect(page.locator('#frame-stats')).not_to_be_empty(timeout=30000)
            console = page.locator('#console').inner_text()
            assert 'life stopped:' not in console, console
            assert not page.locator('#console .error').count(), console
            print(f'{backend}: {console}', flush=True)
            page.screenshot(path=str(OUT / f'playground-{backend}.png'))
            page.locator('#stop').click()
        assert not errors, errors
        assert not failed, failed
        browser.close()
        print('PASS: published playground, numerical results, GPU animation, no failed requests or page errors.', flush=True)


if __name__ == '__main__':
    main()
