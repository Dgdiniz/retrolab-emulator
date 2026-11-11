"""Serve `dist/` as plain static files, regardless of which Vite build
produced it.

Pairs with both:
  * `npm run watch`  -> Vite emits root-relative URLs (`/assets/...`),
                       so we serve `dist/` at `http://localhost:8000/`.
  * `npm run build`  -> Vite emits subpath URLs (`/retrolab-emulator/...`)
                       for the GitHub Pages deploy. We detect the prefix
                       from `dist/index.html` and rewrite incoming paths
                       so the same files are still served correctly,
                       just under that prefix. A request to `/` redirects
                       to the prefixed URL so opening
                       `http://localhost:8000/` works either way.

No symlinks, no temp directories, no extra deps: only Python's
`http.server` subclassed.
"""

from __future__ import annotations

import os
import re
import sys
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

PORT = 8000
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
WEB_DIR = os.path.dirname(SCRIPT_DIR)
DIST_DIR = os.path.join(WEB_DIR, "dist")
INDEX_PATH = os.path.join(DIST_DIR, "index.html")


def detect_base() -> str:
    """Return the base prefix encoded in `dist/index.html`, or `''`.

    The Vite output always references its first hashed asset via either
    `href=".../assets/..."` or `src=".../assets/..."`. The portion before
    `/assets/` is the build's base prefix.
    """
    if not os.path.exists(INDEX_PATH):
        print(
            "error: dist/index.html not found. Run `npm run build` "
            "or `npm run watch` first.",
            file=sys.stderr,
        )
        sys.exit(1)
    with open(INDEX_PATH, encoding="utf-8") as fh:
        html = fh.read()
    match = re.search(r'(?:href|src)="([^"]*?)/assets/', html)
    return match.group(1) if match else ""


BASE = detect_base()


class Handler(SimpleHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (BaseHTTPRequestHandler API)
        # Friendly redirect: opening the bare origin lands at the prefix
        # so users don't have to remember the GH Pages subpath.
        if BASE and self.path in ("/", ""):
            self.send_response(302)
            self.send_header("Location", BASE + "/")
            self.end_headers()
            return
        super().do_GET()

    def translate_path(self, path: str) -> str:
        # Strip the base prefix from incoming URLs before resolving.
        # Requests for `/retrolab-emulator/assets/foo.js` are served
        # from `dist/assets/foo.js`.
        if BASE:
            if path.startswith(BASE + "/"):
                path = path[len(BASE):]
            elif path == BASE:
                path = "/"
        return super().translate_path(path)


def main() -> None:
    url = f"http://localhost:{PORT}{BASE}/"
    label = "production build (subpath)" if BASE else "development build (root)"
    print(f"Detected {label}.")
    print(f"Serving {DIST_DIR}")
    print(f"  -> {url}")
    print("Press Ctrl+C to stop.")
    handler = partial(Handler, directory=DIST_DIR)
    server = ThreadingHTTPServer(("", PORT), handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print()
        server.server_close()


if __name__ == "__main__":
    main()
