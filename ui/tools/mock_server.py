#!/usr/bin/env python3
"""A stand-in for `lunacy run`'s HTTP API, for screenshots and manual poking.

Implements every endpoint in `docs/api.md`, including the shapes that are easy to get wrong: a
running run with live act substeps, one with a `github_url` and one without, steps skipped by
`only_on`, a config with masked secrets, and a PUT that rejects a bad field with a 400 + `field`.

    python3 tools/mock_server.py --port 7878 --token dev

Then point the UI at it (`--server http://127.0.0.1:7878 --token dev`). `--token ""` disables
auth. Fixtures live in `tools/fixtures.json`. A PUT to `/api/config` is validated the way the
daemon validates it and echoed back, so the editor can be driven end to end.
"""

import argparse
import threading
import copy
import json
import os
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, cast
from urllib.parse import parse_qs, unquote, urlparse

HERE = os.path.dirname(os.path.abspath(__file__))

Json = dict[str, Any]

DONE = ("success", "failure", "timeout", "skipped")
OS_NAMES = ("linux", "windows", "macos")


def load_fixtures() -> Json:
    path = os.path.join(HERE, "fixtures.json")
    try:
        with open(path, encoding="utf-8") as f:
            return cast(Json, json.load(f))
    except OSError as e:
        raise SystemExit("cannot read %s: %s" % (path, e)) from e
    except json.JSONDecodeError as e:
        raise SystemExit("%s is not valid JSON: %s" % (path, e)) from e


def _int(raw: str | None, default: int, *, lo: int, hi: int) -> int:
    """A query number, clamped; a non-number falls back to the default instead of a 500."""
    try:
        return max(lo, min(int(raw or default), hi))
    except (TypeError, ValueError):
        return default


def _rebase_timestamps(fixtures: Json) -> None:
    """Shift the fixture runs to end "a few minutes ago".

    The fixtures are checked in once and served years later, so their absolute times would make
    every screen read "365d ago". Spacing between runs is kept.
    """
    runs = fixtures.get("runs") or []
    # The projects list carries a copy of its latest run, so those timestamps count too.
    snapshots = [
        run
        for project in fixtures.get("projects") or []
        for run in (project.get("current_run"), project.get("last_run"))
        if run
    ]
    stamped = runs + snapshots
    newest = max((r.get("started_at") or 0 for r in stamped), default=0)
    if newest <= 0:
        return
    target = int(time.time()) - 240
    shift = target - newest

    def move(t: int) -> int:
        return max(1, t + shift)

    for run in stamped:
        run["started_at"] = move(run.get("started_at") or newest)
        if run.get("finished_at"):
            run["finished_at"] = move(run["finished_at"])


class Store:
    """The mutable half: a run progresses while the server runs, so the UI has something to poll."""

    def __init__(self, fixtures: Json, token: str) -> None:
        _rebase_timestamps(fixtures)
        self.data = fixtures
        self.token = token
        self.config: Json = copy.deepcopy(fixtures["config"])
        self.started = time.time()
        self.logs: dict[str, str] = fixtures.get("logs", {})
        # The running run advances one step every few seconds, then finishes, so a 1 s poll sees
        # pending → running → done instead of a frozen screenshot.
        self.live: Json | None = next(
            (r for r in fixtures["runs"] if r["status"] == "running"), None
        )
        self.failing_step: str = fixtures.get("failing_step", "")
        # ThreadingHTTPServer runs a handler per thread, and each one calls `advance`.
        self.lock = threading.Lock()

    def advance(self) -> None:
        with self.lock:
            self._advance()

    def _advance(self) -> None:
        run = self.live
        if run is None or run["status"] != "running":
            return
        elapsed = time.time() - self.started
        steps: list[Json] = run["steps"]
        want = min(len(steps), int(elapsed // 4))
        for i, step in enumerate(steps):
            if i < want:
                if step["status"] not in DONE:
                    if self.failing_step and step["name"] == self.failing_step:
                        step["status"] = "failure"
                        step["exit_code"] = 1
                        run["status"] = "failure"
                        run["failed_step"] = step["name"]
                        run["finished_at"] = int(time.time())
                    else:
                        step["status"] = "success"
                        step["exit_code"] = 0
                    step["duration_ms"] = 4_000
                    for sub in step.get("substeps") or []:
                        sub["status"] = "success"
            elif i == want and run["status"] == "running":
                step["status"] = "running"
                for sub in step.get("substeps") or []:
                    sub["status"] = "running"
        if run["status"] == "running" and all(s["status"] in DONE for s in steps):
            run["status"] = "success"
            run["finished_at"] = int(time.time())
        if run["finished_at"]:
            self.live = None

    def run_by_id(self, run_id: str) -> Json | None:
        for run in self.data["runs"]:
            if run["id"] == run_id:
                return run
        return None

    def project(self, name: str) -> Json | None:
        for project in self.data["projects"]:
            if project["name"] == name:
                return project
        return None


class MockServer(ThreadingHTTPServer):
    """The store and verbosity live on the server, so the handler is stateless.

    Attributes are set on the instance in `main`; the handler reaches them through `Handler.mock`
    rather than narrowing the base class's `server` annotation, which is invariant.
    """

    store: Store
    verbose: bool
    static: str | None

    def __init__(self, address: tuple[str, int], handler: type[BaseHTTPRequestHandler]) -> None:
        super().__init__(address, handler)
        self.verbose = False


class Handler(BaseHTTPRequestHandler):
    server_version = "lunacy-mock/0.1"

    @property
    def mock(self) -> MockServer:
        """The server, narrowed. `BaseHTTPRequestHandler.server` is a `BaseServer`."""
        return cast(MockServer, self.server)

    # -- plumbing ------------------------------------------------------------------

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 (base signature)
        if self.mock.verbose:
            sys.stderr.write("%s %s\n" % (self.address_string(), format % args))

    def _cors(self) -> None:
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "Authorization, Content-Type")
        self.send_header("Access-Control-Allow-Methods", "GET, PUT, POST, OPTIONS")

    def _send(self, status: int, body: Any, ctype: str = "application/json") -> None:
        if isinstance(body, (dict, list)):
            raw = json.dumps(body, indent=2).encode()
        elif isinstance(body, str):
            raw = body.encode()
        else:
            raw = bytes(body)
        self.send_response(status)
        self._cors()
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def _error(self, status: int, message: str, field: str | None = None) -> None:
        self._send(status, {"error": message, "field": field})

    def _authed(self) -> bool:
        token = self.mock.store.token
        if not token:
            return True
        if self.headers.get("Authorization", "") == "Bearer " + token:
            return True
        self._error(401, "unauthorized")
        return False

    def do_OPTIONS(self) -> None:
        self.send_response(204)
        self._cors()
        self.end_headers()

    def do_GET(self) -> None:
        self.route("GET")

    def do_PUT(self) -> None:
        self.route("PUT")

    def do_POST(self) -> None:
        self.route("POST")

    def _static(self, path: str) -> bool:
        """Serve `--static`'s files; returns False when there is nothing to serve."""
        root = getattr(self.mock, "static", None)
        if not root:
            return False
        rel = "index.html" if path in ("", "/") else path.lstrip("/")
        try:
            target = os.path.realpath(os.path.join(root, rel))
        except OSError:
            self._error(404, "not found")
            return True
        if os.path.commonpath([root, target]) != root:
            self._error(404, "not found")  # a traversal attempt
            return True
        if not os.path.isfile(target):
            self._error(404, "not found")
            return True
        ctype = {
            ".html": "text/html; charset=utf-8",
            ".js": "text/javascript; charset=utf-8",
            ".wasm": "application/wasm",
            ".css": "text/css; charset=utf-8",
            ".ttf": "font/ttf",
            ".json": "application/json",
            ".png": "image/png",
        }.get(os.path.splitext(target)[1], "application/octet-stream")
        with open(target, "rb") as fh:
            body = fh.read()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)
        return True

    # -- routing -------------------------------------------------------------------

    def route(self, method: str) -> None:
        parsed = urlparse(self.path)
        path = unquote(parsed.path)
        query = {k: v[-1] for k, v in parse_qs(parsed.query).items()}
        store = self.mock.store
        store.advance()

        if path == "/api/health":
            self._send(200, {"ok": True, "version": store.data["version"]})
            return
        if not path.startswith("/api/"):
            if self._static(path):
                return
            self._error(404, "not found")
            return
        if not self._authed():
            return

        body: Json | None = None
        length = _int(self.headers.get("Content-Length"), 0, lo=0, hi=64 << 20)
        if length:
            try:
                body = json.loads(self.rfile.read(length))
            except json.JSONDecodeError as e:
                self._error(400, "invalid JSON: %s" % e)
                return

        if path == "/api/projects":
            self._send(200, store.data["projects"])
            return
        if path == "/api/config":
            if method == "PUT":
                self.put_config(body or {})
            else:
                self._send(200, store.config)
            return
        if path == "/api/system":
            self._send(200, store.data["system"])
            return

        m = re.fullmatch(r"/api/projects/([^/]+)/runs", path)
        if m:
            name = m.group(1)
            if store.project(name) is None:
                self._error(404, "unknown project")
                return
            limit = _int(query.get("limit"), 20, lo=1, hi=200)
            runs = [r for r in store.data["runs"] if r["project"] == name]
            self._send(200, runs[:limit])
            return
        m = re.fullmatch(r"/api/projects/([^/]+)/workflows", path)
        if m:
            if m.group(1) != "_" and not query.get("repo_path") and store.project(m.group(1)) is None:
                self._error(404, "unknown project")
                return
            self._send(200, store.data["workflows"])
            return
        m = re.fullmatch(r"/api/projects/([^/]+)/trigger", path)
        if m:
            if method != "POST":
                self._error(405, "POST only")
                return
            project = store.project(m.group(1))
            if project is None:
                self._error(404, "unknown project")
                return
            if project.get("running") and not (body or {}).get("force"):
                self._error(409, "already running")
                return
            self._send(202, {"queued": True})
            return
        m = re.fullmatch(r"/api/projects/([^/]+)", path)
        if m:
            project = store.project(m.group(1))
            if project is None:
                self._error(404, "unknown project")
                return
            self._send(200, project)
            return
        m = re.fullmatch(r"/api/runs/([^/]+)/steps/(\d+)/log", path)
        if m:
            run = store.run_by_id(m.group(1))
            if run is None:
                self._error(404, "unknown run")
                return
            index = int(m.group(2))
            if index >= len(run["steps"]):
                self._error(404, "no such step")
                return
            text = store.logs.get("%s/%d" % (run["id"], index), "no output recorded for this step\n")
            tail = query.get("tail")
            if tail:
                lines = _int(tail, 0, lo=0, hi=100_000)
                if lines:
                    text = "\n".join(text.splitlines()[-lines:]) + "\n"
            self._send(200, text, "text/plain; charset=utf-8")
            return
        m = re.fullmatch(r"/api/runs/([^/]+)", path)
        if m:
            run = store.run_by_id(m.group(1))
            if run is None:
                self._error(404, "unknown run")
                return
            self._send(200, run)
            return
        self._error(404, "unknown endpoint")

    # -- config validation ---------------------------------------------------------

    def put_config(self, body: Json) -> None:
        """The daemon's validation, as far as the UI can trip it."""
        store = self.mock.store
        projects: list[Json] = body.get("project") or []
        seen = set()
        for i, project in enumerate(projects):
            name = (project.get("name") or "").strip()
            if not name:
                self._error(400, "project name is required", "project[%d].name" % i)
                return
            if name in seen:
                self._error(400, "duplicate project name %r" % name, "project[%d].name" % i)
                return
            seen.add(name)
            for phase in ("ci", "deploy"):
                steps: list[Json] = project.get(phase) or []
                for j, step in enumerate(steps):
                    prefix = "project[%d].%s[%d]" % (i, phase, j)
                    if not (step.get("name") or "").strip():
                        self._error(400, "step name is required", prefix + ".name")
                        return
                    if (step.get("kind") or "shell") == "act":
                        if not (step.get("workflow") or "").strip():
                            self._error(400, "act step needs a workflow", prefix + ".workflow")
                            return
                    elif not (step.get("run") or "").strip():
                        self._error(400, "step needs a run line", prefix + ".run")
                        return
                    for os_name in step.get("only_on") or []:
                        if os_name not in OS_NAMES:
                            self._error(
                                400, "only_on takes linux, windows or macos", prefix + ".only_on"
                            )
                            return
        store.config = body
        self._send(200, body)


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--port", type=int, default=7878)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--token", default="", help="require this bearer token (default: no auth)")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument(
        "--static",
        metavar="DIR",
        help="also serve a web build (e.g. dist) on the same origin, so the browser client "
        "needs no proxy",
    )
    args = ap.parse_args()
    server = MockServer((args.host, args.port), Handler)
    server.static = os.path.abspath(args.static) if args.static else None
    server.store = Store(load_fixtures(), args.token)
    server.verbose = args.verbose
    print(
        "mock lunacy API on http://%s:%d (token: %s)"
        % (args.host, args.port, args.token or "none"),
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print()


if __name__ == "__main__":
    main()
