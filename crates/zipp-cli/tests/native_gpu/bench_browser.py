"""ZIPP's GPU runtime (gpu-lab) in headless Chrome: the benchmark cases of
bench_vs_torch.py as graph programs, timed by crates/zipp-gpu/tests/engine_bench.js,
the same function the native engine benchmark runs over wgpu.

    py -3.11 bench_browser.py [OUT.json] [--headed]

Writes records bench_vs_torch.py merges into its table (`--browser OUT.json`).
Serves crates/zipp-wasm/gpu-lab over http://127.0.0.1 (a secure context) as
gpu-lab's browser_smoke.py does; uses installed Chrome (PLAYWRIGHT_CHANNEL,
default "chrome") or CHROMIUM_BIN.
"""
import functools
import http.server
import json
import os
import sys
import threading
from pathlib import Path
from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
LAB = REPO / "crates/zipp-wasm/gpu-lab"
BENCH = REPO / "crates/zipp-gpu/tests/engine_bench.js"

# What each backend runs: the WebGPU backend everything, the others what
# finishes in reasonable time.
PLAN = {
    "webgpu": {},
    "webgl2": {"only": ["mlp_s", "mlp_m", "mm_1024", "mm_2048", "ew_4m", "emb"], "calls": 3},
    "wasm": {"only": ["mlp_s", "mm_1024", "ew_4m"], "reps": 5, "runs": 2, "calls": 2},
}


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map,
                      ".mjs": "text/javascript", ".js": "text/javascript", ".wasm": "application/wasm"}

    def log_message(self, *args):
        pass


def main():
    out_path = next((a for a in sys.argv[1:] if not a.startswith("--")), None)
    backends = os.environ.get("BENCH_BACKENDS", "webgpu,webgl2,wasm").split(",")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(LAB)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = "http://127.0.0.1:%d" % server.server_address[1]
    flags = os.environ.get("GPU_FLAGS", "--force_high_performance_gpu").split()
    options = {"headless": "--headed" not in sys.argv, "args": flags}
    if os.environ.get("CHROMIUM_BIN"):
        options["executable_path"] = os.environ["CHROMIUM_BIN"]
    else:
        options["channel"] = os.environ.get("PLAYWRIGHT_CHANNEL", "chrome")
    records = []
    with sync_playwright() as p:
        browser = p.chromium.launch(**options)
        page = browser.new_page()
        page.set_default_timeout(0)
        page.goto(base + "/tests/")
        page.add_script_tag(path=str(BENCH))
        for backend in backends:
            result = page.evaluate("""async ([backend, opts]) => {
              const R = await import('/src/runtime.mjs'), C = await import('/tests/ml-cases.mjs');
              try { return await engineBench({...R, ...C}, backend, opts); }
              catch (e) { return {backend, error: String(e && e.message || e)}; }
            }""", [backend, PLAN.get(backend, {})])
            if "error" in result:
                print(backend, "unavailable:", result["error"], flush=True)
                continue
            adapter = (result.get("info") or {}).get("adapter") or {}
            print(backend, adapter.get("description", ""), flush=True)
            for c in result["cases"]:
                rec = {"case": c["case"], "impl": "zipp-chrome-" + backend, "step_ms": c["stepMs"], "steps8_ms": c["steps8Ms"],
                       "first_step_ms": c["prepareMs"] + c["firstMs"]}
                if "callMs" in c:
                    rec["call_ms"] = c["callMs"]
                if "gflops" in c:
                    rec["gflops"] = c["gflops"]
                records.append(rec)
                print("  %-8s step %8.2f ms  8/run %8.2f ms  per-call %s" % (c["case"], c["stepMs"], c["steps8Ms"],
                      "%.1f ms" % c["callMs"] if "callMs" in c else "-"), flush=True)
        browser.close()
    server.shutdown()
    if out_path:
        Path(out_path).write_text(json.dumps(records, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
