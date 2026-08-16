"""Local HTTP shim in front of a remote HTTPS embedding server.

The lore daemon's reqwest is deliberately pinned without TLS (D-0003: every
endpoint it talks to is loopback), so it cannot reach an https:// endpoint
directly. When the embedding model must run off-box (e.g. the local GPU is
busy hosting the bench's causal model), run this forwarder and point
config.toml at it:

    python scripts/embed-remote-proxy.py --target https://<pod>.proxy.runpod.net
    # config.toml: endpoint = "http://127.0.0.1:8091/v1"

Paths pass through unchanged (/v1/embeddings -> {target}/v1/embeddings).
A curl-ish User-Agent is set on purpose: RunPod's edge proxy 403s the
default Python-urllib UA (observed 2026-08-16).
"""

import argparse
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8091)
    ap.add_argument("--target", required=True, help="remote base URL, no trailing slash")
    ap.add_argument("--timeout", type=float, default=120.0)
    args = ap.parse_args()
    target = args.target.rstrip("/")

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            req = urllib.request.Request(
                target + self.path,
                body,
                {
                    "Content-Type": self.headers.get("Content-Type", "application/json"),
                    "User-Agent": "curl/8.9.1 (lore embed-remote-proxy)",
                },
            )
            try:
                with OPENER.open(req, timeout=args.timeout) as resp:
                    data = resp.read()
                    status = resp.status
            except urllib.error.HTTPError as err:
                data = err.read()
                status = err.code
            except OSError as err:
                data = str(err).encode()
                status = 502
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, fmt, *a):
            pass  # the daemon retries chatter into this log otherwise

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"forwarding http://127.0.0.1:{args.port} -> {target}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
