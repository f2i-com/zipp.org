"""Static localhost-only server. Does not execute submitted Python or accept writes."""
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import argparse

ROOT=Path(__file__).resolve().parents[1]
class Handler(SimpleHTTPRequestHandler):
    extensions_map={**SimpleHTTPRequestHandler.extensions_map,".mjs":"text/javascript",".wasm":"application/wasm"}
    def do_GET(self):
        if self.path=="/":
            self.send_response(302);self.send_header("Location","/demo/");self.end_headers();return
        super().do_GET()
    def end_headers(self):
        self.send_header("X-Content-Type-Options","nosniff")
        self.send_header("Cache-Control","no-store")
        super().end_headers()

if __name__=="__main__":
    parser=argparse.ArgumentParser();parser.add_argument("--port",type=int,default=8765);args=parser.parse_args()
    try:server=ThreadingHTTPServer(("127.0.0.1",args.port),partial(Handler,directory=str(ROOT)))
    except OSError as exc:parser.exit(1,"Could not start the local server: %s\n"%exc)
    print("Open http://localhost:%d/demo/"%args.port,flush=True)
    print("Stop with Ctrl+C. All code and examples are local; no CDN dependencies.",flush=True)
    try:server.serve_forever()
    except KeyboardInterrupt:pass
    finally:server.server_close()
