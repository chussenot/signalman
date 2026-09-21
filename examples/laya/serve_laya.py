"""A System One-compatible HTTP shim over Laya, for experiments.

Laya (https://huggingface.co/convaiinnovations/laya, Apache 2.0) answers the
same typed questions as TypeSafe's System One API, in one forward pass, from
open weights. This shim exposes it on the two endpoints signalman calls, so
the binary runs unchanged with ``typesafe.base_url`` pointed here:

    python -m venv .venv && .venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu
    .venv/bin/pip install laya
    USE_TF=0 .venv/bin/python examples/laya/serve_laya.py            # English checkpoint, port 8099
    LAYA_SUBFOLDER=typed-decisions LAYA_PORT=8100 .venv/bin/python examples/laya/serve_laya.py

    TYPESAFE_BASE_URL=http://127.0.0.1:8099 TYPESAFE_API_KEY=unused signalman triage examples/alerts/crashloop.json

Endpoints: ``POST /v1/systemone`` ({state, model?, questions} -> {model,
answers, usage}) and ``GET /v1/models``. The bearer token is accepted and
ignored. Not a product: no auth, no TLS, one process, no batching across
requests. See docs/laya.md for what was measured and why Jev stays the
default.
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
