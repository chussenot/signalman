"""A System One-compatible HTTP shim over Laya, for experiments.

Laya (https://huggingface.co/convaiinnovations/laya, Apache 2.0) answers the
same typed questions as TypeSafe's System One API, in one forward pass, from
open weights. This shim exposes one checkpoint on the two endpoints the
judgment crate calls, ``POST /v1/systemone`` and ``GET /v1/models``, so the
crate's live tests and examples, and any System One client pointed here
(signalman among them), run unchanged. From the crate's root directory:

    python -m venv .venv && .venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu
    .venv/bin/pip install laya
    USE_TF=0 .venv/bin/python examples/laya/serve_laya.py            # English checkpoint, port 8099
    LAYA_SUBFOLDER=typed-decisions LAYA_PORT=8100 .venv/bin/python examples/laya/serve_laya.py

    JUDGMENT_LIVE_BASE_URL=http://127.0.0.1:8100 cargo test -p judgment --test live -- --ignored --nocapture

Laya ships its own server, ``laya-serve`` (``pip install "laya[serve]"``),
which is what docs/verification/laya-typed-decisions.md runs the crate
against; prefer it. This shim stays for the one path laya-serve does not
serve, ``GET /v1/models``, so a model listing has something to list, and as
the smallest reference of the wire. The checkpoint is chosen when the shim
starts and the request's ``model`` field is ignored; the response names it
``laya:<subfolder>``. The bearer token is accepted and ignored. A request
Laya raises on is a 400 with ``{"error": {"message", "type"}}``, which the
crate reads as ``Error::InvalidRequest`` and does not retry. Structured
Score levels come back in the legend as the JSON text ``laya`` 0.3.22 and
later render them with, as from laya-serve. Not a product: no auth, no TLS,
one process, no batching across requests. What signalman measured through
it, and why Jev stays its default, is at
https://github.com/chussenot/signalman/blob/main/docs/laya.md.
"""
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

os.environ.setdefault("USE_TF", "0")  # transformers may deadlock probing TensorFlow
import laya  # noqa: E402

REPO = os.environ.get("LAYA_REPO", "convaiinnovations/laya")
SUBFOLDER = os.environ.get("LAYA_SUBFOLDER") or None
PORT = int(os.environ.get("LAYA_PORT", "8099"))

started = time.time()
agent = laya.load(REPO, subfolder=SUBFOLDER) if SUBFOLDER else laya.load(REPO)
MODEL = f"laya:{SUBFOLDER or 'english'}"
print(f"loaded {MODEL} in {time.time() - started:.1f}s", file=sys.stderr, flush=True)


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.startswith("/v1/models"):
            return self._send(200, {"models": [{"name": MODEL, "description": "Laya, local shim", "release_date": "2026-09-20"}]})
        self._send(404, {"error": {"message": "not found", "type": "not_found"}})

    def do_POST(self):
        if not self.path.startswith("/v1/systemone"):
            return self._send(404, {"error": {"message": "not found", "type": "not_found"}})
        length = int(self.headers.get("Content-Length", "0"))
        request = json.loads(self.rfile.read(length) or b"{}")
        t = time.time()
        try:
            result = agent.predict(request["state"], request["questions"])
        except Exception as e:  # a 400 is not retried by the client
            return self._send(400, {"error": {"message": str(e), "type": "invalid_request"}})
        result["model"] = MODEL
        result.setdefault("usage", {"input_tokens": 0, "output_tokens": 0})
        print(f"systemone {len(request['questions'])} questions in {(time.time() - t) * 1000:.0f} ms", file=sys.stderr, flush=True)
        self._send(200, result)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
