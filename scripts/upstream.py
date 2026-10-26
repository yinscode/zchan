# A stand-in "existing API" for the proxy-mode demo: /free, GET /api/search?q=, POST /api/echo.
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

class H(BaseHTTPRequestHandler):
    def reply(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        u = urlparse(self.path)
        if u.path == "/free": return self.reply(200, {"free": True})
        if u.path == "/api/search":
            return self.reply(200, {"q": parse_qs(u.query).get("q", [""])[0], "headers": sorted(k.lower() for k in self.headers.keys())})
        self.reply(404, {"error": "not found"})
    def do_POST(self):
        n = int(self.headers.get("content-length", 0)); body = self.rfile.read(n).decode()
        if urlparse(self.path).path == "/api/echo":
            return self.reply(200, {"echo": body, "headers": sorted(k.lower() for k in self.headers.keys())})
        self.reply(404, {"error": "not found"})
    def log_message(self, *a): pass

ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
