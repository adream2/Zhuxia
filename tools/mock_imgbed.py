import json
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

PORT = 8799
STORE = {}
LOCK = threading.Lock()


def xml_escape(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def _send(self, code, body=b"", ctype="application/octet-stream", extra=None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_OPTIONS(self):
        self._send(200, b"", "text/plain", {"Allow": "GET,PUT,POST,DELETE,PROPFIND,MKCOL,OPTIONS"})

    def do_MKCOL(self):
        self._send(201, b"")

    def _json(self, obj, code=200):
        self._send(code, json.dumps(obj).encode(), "application/json")

    def do_PUT(self):
        body = self._body()
        if self.path.startswith("/gh/"):
            try:
                data = json.loads(body or b"{}")
            except Exception:
                data = {}
            content = data.get("content") or ""
            import base64 as b64
            try:
                size = len(b64.b64decode(content))
            except Exception:
                size = 0
            path = self.path.split("/contents/", 1)[-1]
            self._json({"content": {"path": path, "size": size, "sha": "c0ffee"}, "commit": {"sha": "beef"}})
            return
        with LOCK:
            STORE[self.path] = body
        self._send(201, b"", "text/plain", {"ETag": '"mock"'})

    def do_DELETE(self):
        if self.path.startswith("/gh/"):
            self._send(200, b"{}", "application/json")
            return
        with LOCK:
            STORE.pop(self.path, None)
        self._send(204, b"")

    def do_POST(self):
        body = self._body()
        name = "upload.webp"
        m = re.search(rb'filename="([^"]+)"', body)
        if m:
            name = m.group(1).decode("utf-8", "replace")
        m2 = re.search(rb"\r\n\r\n(.+)\r\n--", body, re.S)
        data = m2.group(1) if m2 else b""
        key = "/files/" + name
        with LOCK:
            STORE[key] = data
        out = json.dumps({"status": True, "data": {"links": {"url": f"http://127.0.0.1:{PORT}{key}"}}}).encode()
        self._send(200, out, "application/json")

    def do_PROPFIND(self):
        body = self._body()
        path = urlparse(self.path).path.rstrip("/")
        with LOCK:
            keys = list(STORE.keys())
        children = {}
        prefix = path + "/"
        for k in keys:
            if k.startswith(prefix):
                rest = k[len(prefix):]
                seg = rest.split("/")[0]
                children[seg] = "/" in rest
        items = []
        items.append(
            f'<d:response><d:href>{xml_escape(path)}/</d:href><d:propstat><d:prop>'
            f'<d:resourcetype><d:collection/></d:resourcetype></d:prop>'
            f'<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>'
        )
        for name, is_dir in children.items():
            href = f"{path}/{name}" + ("/" if is_dir else "")
            if is_dir:
                prop = "<d:resourcetype><d:collection/></d:resourcetype>"
            else:
                size = len(STORE.get(prefix + name, b""))
                prop = f"<d:resourcetype/><d:getcontentlength>{size}</d:getcontentlength>"
            items.append(
                f'<d:response><d:href>{xml_escape(href)}</d:href><d:propstat><d:prop>'
                f"{prop}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
            )
        xml = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:multistatus xmlns:d="DAV:">' + "".join(items) + "</d:multistatus>"
        ).encode()
        self._send(207, xml, "application/xml; charset=utf-8")

    def do_GET(self):
        u = urlparse(self.path)
        if u.path.startswith("/gh/"):
            p = u.path
            if p == "/gh/user":
                self._json({"login": "mock"})
            elif p.endswith("/git/ref/heads/main"):
                self._json({"object": {"sha": "abc123"}})
            elif "/git/commits/" in p:
                self._json({"tree": {"sha": "tree123"}})
            elif re.search(r"/repos/[^/]+/[^/]+$", p):
                self._json({"full_name": "o/r", "private": False, "default_branch": "main",
                            "permissions": {"push": True}, "size": 12})
            elif "/contents" in p:
                self._json([
                    {"type": "dir", "name": "img", "path": "img"},
                    {"type": "file", "name": "a.webp", "path": "img/a.webp", "size": 123},
                ])
            else:
                self._json({"message": "not found"}, 404)
            return
        q = parse_qs(u.query)
        path = u.path
        if "list-type" in q:
            prefix = (q.get("prefix") or [""])[0]
            with LOCK:
                pairs = []
                for stored, data in STORE.items():
                    disp = stored[len("/bkt/"):] if stored.startswith("/bkt/") else stored
                    pairs.append((disp, stored, len(data)))
            dirs = set()
            files = []
            for disp, stored, size in sorted(pairs):
                if not disp.startswith(prefix) or disp == prefix:
                    continue
                rest = disp[len(prefix):]
                if "/" in rest:
                    dirs.add(prefix + rest.split("/")[0] + "/")
                else:
                    files.append((disp, size))
            parts = ['<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">']
            for d in sorted(dirs):
                parts.append(f"<CommonPrefixes><Prefix>{xml_escape(d)}</Prefix></CommonPrefixes>")
            for k, n in files:
                parts.append(f"<Contents><Key>{xml_escape(k)}</Key><Size>{n}</Size></Contents>")
            parts.append("</ListBucketResult>")
            self._send(200, "".join(parts).encode(), "application/xml")
            return
        with LOCK:
            data = STORE.get(path)
        if data is None:
            self._send(404, b"not found", "text/plain")
            return
        ctype = "image/webp"
        if path.endswith(".png"):
            ctype = "image/png"
        elif path.endswith(".jpg"):
            ctype = "image/jpeg"
        self._send(200, data, ctype)


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"mock listening on {PORT}", flush=True)
    srv.serve_forever()
