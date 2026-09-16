"""Browser validation using Playwright: every numerical case on each backend
against the JavaScript reference, plus an MNIST-scale MLP training step.

The lab is served from http://127.0.0.1 (a secure context, so WebGPU is
exposed) and the page imports the ES modules directly. Unavailable GPU
backends are reported as unavailable, never replaced by a CPU pass; with
REQUIRE_GPU=1 they fail the run. Uses installed Chrome (PLAYWRIGHT_CHANNEL,
default "chrome") or CHROMIUM_BIN. Pass --headed to show the browser.
"""
import functools
import http.server
import json
import os
import sys
import threading
from datetime import datetime, timezone
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map,
                      ".mjs": "text/javascript", ".js": "text/javascript", ".wasm": "application/wasm"}

    def log_message(self, *args):
        pass


def render(result):
    """JSON with one line per numerical check, so the recorded evidence stays reviewable."""
    parts = []
    for key, value in result.items():
        if key == "reports":
            reports = []
            for report in value:
                head = {k: v for k, v in report.items() if k != "checks"}
                rows = ",\n        ".join(json.dumps(c, separators=(",", ":")) for c in report["checks"])
                reports.append('    {"summary": %s,\n      "checks": [\n        %s]}' % (json.dumps(head), rows))
            parts.append('  "reports": [\n%s\n  ]' % ",\n".join(reports))
        else:
            parts.append("  %s: %s" % (json.dumps(key), json.dumps(value)))
    return "{\n" + ",\n".join(parts) + "\n}\n"


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(ROOT)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = "http://127.0.0.1:%d" % server.server_address[1]
    # Chrome's default adapter choice on hybrid-GPU machines varies between runs;
    # ask for the discrete GPU unless GPU_FLAGS overrides it (GPU_FLAGS="" for the default).
    flags = os.environ.get("GPU_FLAGS", "--force_high_performance_gpu").split()
    options = {"headless": "--headed" not in sys.argv, "args": flags}
    if os.environ.get("CHROMIUM_BIN"):
        options["executable_path"] = os.environ["CHROMIUM_BIN"]
    else:
        options["channel"] = os.environ.get("PLAYWRIGHT_CHANNEL", "chrome")
    with sync_playwright() as p:
        browser = p.chromium.launch(**options)
        page = browser.new_page()
        page.goto(base + "/tests/")
        reports, training, nan = [], [], []
        for backend in ["webgpu", "webgl2", "wasm", "cpu-js"]:
            report = page.evaluate("""async backend => {
              const {checkBackend} = await import('/tests/browser-cases.mjs');
              return checkBackend(backend);
            }""", backend)
            reports.append(report)
            worst = max([c.get("maxAbsError", 0) for c in report["checks"]] or [0])
            print(backend, report["status"], "%d/%d" % (report.get("passed", 0), len(report["checks"])), "max abs error %.3g" % worst)
            for c in report["checks"]:
                if c["status"] != "passed":
                    print("   FAILED", c["name"], c.get("error"))
            if report["status"] == "unavailable":
                continue
            bench = page.evaluate("""async backend => {
              const {benchmarkTraining} = await import('/tests/browser-cases.mjs');
              return benchmarkTraining(backend);
            }""", backend)
            training.append(bench)
            print("   784-256-10 batch-64 Adam step: cold %.1f ms, warm median %.2f ms (min %.2f), loss-only %.2f ms, batch-512 loss-only %s, max abs error vs cpu-js %.3g, losses %s"
                  % (bench["coldMs"], bench["warmMedianMs"], bench["warmMinMs"], bench["lossOnlyWarmMedianMs"], bench["batch512LossOnlyWarmMedianMs"], bench["maxAbsError"],
                     " ".join("%.4f" % v for v in bench["losses"])))
            print("   typed outputs, per step: execute() %.2f ms, session 1 step/run %.2f ms, session 8 steps/run %.2f ms; five session steps vs chained executes max abs error %.3g"
                  % (bench["executeTypedWarmMedianMs"], bench["sessionOneStepWarmMedianMs"], bench["sessionEightStepsPerStepMedianMs"], bench["sessionVsChainedMaxAbsError"]))
            # Non-finite intermediates are rejected at readback where the platform propagates NaN.
            nan.append(page.evaluate("""async backend => {
              const {createRuntime} = await import('/src/runtime.mjs');
              const rt = await createRuntime({backend}), result = {backend};
              for (const op of ['relu', 'sum', 'tanh', 'gelu', 'softmax', 'matmul']) {
                const nodes = [{id:0,op:'input',shape:[1,2],data:[3e38,1]},{id:1,op:'full',shape:[],value:10},
                  {id:2,op:'mul',a:0,b:1},{id:3,op:'sub',a:2,b:2},
                  ...(op === 'matmul' ? [{id:4,op:'input',shape:[2,1],data:[1,1]},{id:5,op,a:3,b:4}] : [{id:4,op,a:3}])];
                try { await rt.execute({version:2,nodes,outputs:[{name:'r',id:nodes.length-1}]}); result[op] = 'finite'; }
                catch (e) { result[op] = e.code; }
              }
              rt.dispose(); return result;
            }""", backend))
            print("   NaN intermediate ->", {k: v for k, v in nan[-1].items() if k != "backend"})
        result = {"testedAt": datetime.now(timezone.utc).isoformat(), "browser": browser.version,
                  "mode": "Local HTTP origin (%s), headless=%s, flags=%s; ES modules imported directly" % (base, options["headless"], flags),
                  "reports": reports, "training": training, "nanIntermediates": nan}
        (ROOT / "docs/browser-validation.json").write_text(render(result), encoding="utf-8")
        browser.close()
    server.shutdown()
    failed = any(r["status"] == "failed" for r in reports)
    missing = [r["backend"] for r in reports if r["status"] == "unavailable" and r["backend"] in ("webgpu", "webgl2")]
    if missing and os.environ.get("REQUIRE_GPU") == "1":
        print("GPU backends unavailable:", ", ".join(missing))
        failed = True
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
