"""Optional browser tests using Playwright; no network requests or GPU claims on skip.

Install Playwright separately and set CHROMIUM_BIN or install its bundled browser.
This local-source harness tests runtime maths, not real-origin HTTP/Worker loading.
"""
import json
import os
from pathlib import Path
import shutil
import sys
from datetime import datetime, timezone
from playwright.sync_api import sync_playwright
from bundle_for_test import bundle, ROOT

with sync_playwright() as p:
    executable=os.environ.get("CHROMIUM_BIN") or shutil.which("chromium") or shutil.which("google-chrome")
    options={"headless":True,"args":["--no-sandbox","--disable-dev-shm-usage"]}
    if executable:options["executable_path"]=executable
    browser=p.chromium.launch(**options)
    page=browser.new_page(viewport={"width":1440,"height":1240},device_scale_factor=1)
    page.set_content("<!doctype html><html><head></head><body>Local compute runtime validation</body></html>")
    page.evaluate(bundle())
    wasm=list((ROOT/"wasm/kernels.wasm").read_bytes())
    reports=[]
    for backend in ["webgpu","webgl2","wasm","cpu-js"]:
        report=page.evaluate("async ({backend,bytes}) => await ZippGPULab.checkBackend(backend,{wasmBytes:new Uint8Array(bytes)})",{"backend":backend,"bytes":wasm})
        reports.append(report)
        print(backend,report["status"],report.get("passed",0))
    result={"testedAt":datetime.now(timezone.utc).isoformat(),"browser":browser.version,
            "mode":"Offline local-source evaluation on about:blank; no real-origin Worker/HTTP coverage",
            "reports":reports}
    (ROOT/"docs/browser-validation.json").write_text(json.dumps(result,indent=2)+"\n",encoding="utf-8")
    # Visual-only check of local demo layout. It does not assert app/Worker integration.
    html=(ROOT/"demo/index.html").read_text(encoding="utf-8")
    html=html.replace('<link rel="stylesheet" href="./style.css">','<style>'+(ROOT/"demo/style.css").read_text(encoding="utf-8")+'</style>')
    html=html.replace('<script type="module" src="./app.mjs"></script>','')
    page.set_content(html)
    page.evaluate("""() => {
      document.getElementById('pythonCode').textContent='from zipp_gpu import Graph\\n\\ngpu = Graph()\\na = gpu.tensor([1, 2, 3, 4])\\nb = gpu.tensor([10, 20, 30, 40])\\n\\nc = (a * b + 4).relu()\\nprogram = gpu.program(\\n    result=c, total=c.sum()\\n)';
      document.getElementById('resultCode').textContent='Run an example to inspect its computed values.\\n\\nWebGPU / WebGL2 / WASM / JavaScript';
    }""")
    page.screenshot(path=str(ROOT/"docs/demo-layout.png"),full_page=True)
    page.set_viewport_size({"width":390,"height":844})
    overflow=page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
    result["visualMobileOverflow"]=overflow
    (ROOT/"docs/browser-validation.json").write_text(json.dumps(result,indent=2)+"\n",encoding="utf-8")
    browser.close()
if any(r["status"]=="failed" for r in reports) or overflow:sys.exit(1)
