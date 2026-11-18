"""Screenshots of the built site, for review.

Serves out/ on a local port and saves the landing page and one docs page in
dark and light, at desktop and phone width, to the folder given, by default
screens/, which git ignores. The terminal screenshots the docs show are made
by scripts/screens.py and kept in public/screens/.

    npm run build
    python3 scripts/site-screens.py [FOLDER]

Needs Python Playwright. CHROME points to a browser binary; the default is
/usr/bin/google-chrome.
"""

import functools
import http.server
import os
import sys
import socketserver
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "out"
SCREENS = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / "screens"
CHROME = os.environ.get("CHROME", "/usr/bin/google-chrome")

PAGES = {"landing": "/", "docs": "/docs/start/quick-start/"}
SIZES = {"desktop": (1440, 900), "phone": (390, 844)}
THEMES = ("dark", "light")


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def main():
    SCREENS.mkdir(parents=True, exist_ok=True)
    handler = functools.partial(Quiet, directory=str(OUT))
    with socketserver.TCPServer(("127.0.0.1", 0), handler) as server:
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        with sync_playwright() as p:
            browser = p.chromium.launch(executable_path=CHROME)
            for theme in THEMES:
                for size, (width, height) in SIZES.items():
                    context = browser.new_context(
                        viewport={"width": width, "height": height},
                        color_scheme=theme,
                        device_scale_factor=2 if size == "phone" else 1,
                    )
                    page = context.new_page()
                    for name, path in PAGES.items():
                        page.goto(f"http://127.0.0.1:{port}{path}", wait_until="networkidle")
                        page.evaluate("document.fonts.ready")
                        file = SCREENS / f"{name}-{theme}-{size}.png"
                        page.screenshot(path=str(file), full_page=(name == "landing"))
                        print(file)
                    context.close()
            browser.close()
        server.shutdown()


if __name__ == "__main__":
    main()
