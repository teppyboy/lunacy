#!/usr/bin/env python3
"""Repeatable E2E runner for lunacy UI (native Rune + Web WASM) against a real daemon."""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[3]
DAEMON_BIN = ROOT / "target" / "debug" / "lunacy"
UI_BIN = ROOT / "ui" / "target" / "debug" / "lunacy-ui"
WEB_CHECK = ROOT / "ui" / "tools" / "e2e" / "web_check.mjs"
RUNE_DIR = pathlib.Path(
    os.environ.get(
        "RUNE_DIR",
        "/tmp/pi-github-repos/runtime-vsiOuE/c8125861ff163c50577a9cb425c53259a4411a18295dcf09b24a2d2066406eb9",
    )
)
TOKEN = "e2e-token"
WEBHOOK_SECRET = "e2e-webhook-secret"


def free_port() -> int:
    for _ in range(20):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        if port != 7878:
            return port
    raise RuntimeError("failed to find free non-7878 port")


def run_cmd(cmd: list[str], cwd: pathlib.Path | None = None, env: dict[str, str] | None = None) -> str:
    res = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(
            f"command failed ({res.returncode}): {' '.join(cmd)}\nstdout:\n{res.stdout}\nstderr:\n{res.stderr}"
        )
    return res.stdout


def git_init_repo(bare: pathlib.Path, work: pathlib.Path, files: dict[str, str], msg: str) -> None:
    run_cmd(["git", "init", "--bare", "-b", "master", str(bare)])
    work.mkdir(parents=True, exist_ok=True)
    run_cmd(["git", "init", "-b", "master"], cwd=work)
    run_cmd(["git", "config", "user.name", "E2E"], cwd=work)
    run_cmd(["git", "config", "user.email", "e2e@example.invalid"], cwd=work)
    for rel, content in files.items():
        p = work / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
    run_cmd(["git", "add", "."], cwd=work)
    run_cmd(["git", "commit", "-m", msg], cwd=work)
    run_cmd(["git", "remote", "add", "origin", str(bare)], cwd=work)
    run_cmd(["git", "push", "-u", "origin", "master"], cwd=work)


def api_call(
    port: int,
    path: str,
    *,
    method: str = "GET",
    token: str | None = TOKEN,
    json_body: object | None = None,
) -> tuple[int, object]:
    url = f"http://127.0.0.1:{port}{path}"
    data = None if json_body is None else json.dumps(json_body).encode()
    headers: dict[str, str] = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            try:
                return resp.status, json.loads(raw)
            except json.JSONDecodeError:
                return resp.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", errors="replace")
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, raw


def wait_until(pred, *, timeout: float = 30.0, step: float = 0.2, desc: str = "condition"):
    deadline = time.monotonic() + timeout
    last_err = None
    while time.monotonic() < deadline:
        try:
            val = pred()
            if val:
                return val
        except Exception as e:
            last_err = e
        time.sleep(step)
    raise AssertionError(f"timed out waiting for {desc} (last_err={last_err})")


# --------------------------------------------------------------------------- tree query helpers

def elems(tree: dict) -> list[dict]:
    return tree.get("elements", [])


def find_els(tree: dict, key: str) -> list[dict]:
    return [
        el
        for el in elems(tree)
        if el.get("key") == key or str(el.get("path", "")).endswith(f"/{key}")
    ]


def find_el(tree: dict, key: str) -> dict:
    matches = find_els(tree, key)
    if not matches:
        keys = sorted({el.get("key") for el in elems(tree) if el.get("key")})
        raise AssertionError(f"element key {key!r} not found in tree; known keys={keys[:60]}")
    return matches[0]


def has_key(tree: dict, key: str) -> bool:
    return bool(find_els(tree, key))


def subtree_texts(tree: dict, prefix_key: str) -> list[str]:
    root = find_el(tree, prefix_key)
    root_path = root["path"]
    out = []
    for el in elems(tree):
        p = str(el.get("path", ""))
        if p == root_path or p.startswith(root_path + "/"):
            for fld in ("text", "label", "help"):
                val = el.get(fld)
                if val:
                    out.append(str(val))
    return out


def el_rect(el: dict) -> tuple[float, float, float, float]:
    r = el["rect"]
    if isinstance(r, dict):
        return (float(r["x"]), float(r["y"]), float(r["w"]), float(r["h"]))
    return (float(r[0]), float(r[1]), float(r[2]), float(r[3]))


def el_center(el: dict, tree: dict | None = None, window_w: float | None = None) -> dict[str, float]:
    x, y, w, h = el_rect(el)
    scale = 1.0
    if tree is not None and window_w:
        vp = tree.get("viewport") or []
        if vp and vp[0]:
            scale = window_w / float(vp[0])
    return {"x": (x + w / 2.0) * scale, "y": (y + h / 2.0) * scale}


# --------------------------------------------------------------------------- script builder

class Script:
    def __init__(self, t0: float = 1.0) -> None:
        self.t = t0
        self.events: list[dict] = []

    def wait(self, dt: float) -> "Script":
        self.t = round(self.t + dt, 3)
        return self

    def tap(self, key: str, after: float = 0.40) -> "Script":
        self.events.append({"t": self.t, "type": "tap", "key": key})
        return self.wait(after)

    def key(self, name: str, *, cmd: bool = False, shift: bool = False, after: float = 0.30) -> "Script":
        ev: dict[str, object] = {"t": self.t, "type": "key", "name": name}
        if cmd:
            ev["cmd"] = True
        if shift:
            ev["shift"] = True
        self.events.append(ev)
        return self.wait(after)

    def paste(self, text: str, after: float = 0.30) -> "Script":
        self.events.append({"t": self.t, "paste": text})
        return self.wait(after)

    def wheel(self, key: str, dy: float = 0.0, after: float = 0.35, dx: float = 0.0) -> "Script":
        self.events.append({"t": self.t, "type": "wheel", "key": key, "dx": dx, "dy": dy})
        return self.wait(after)

    def mark(self, name: str, after: float = 0.10) -> "Script":
        self.events.append({"t": self.t, "type": "mark", "name": name})
        return self.wait(after)

    def replace(self, field_key: str, value: str, after: float = 0.30) -> "Script":
        self.tap(f"{field_key}/control", after=0.30)
        self.key("a", cmd=True, after=0.25)
        self.key("Backspace", after=0.25)
        if value:
            self.paste(value, after=after)
        else:
            self.wait(after)
        return self


class Sandbox:
    def __init__(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp(dir="/tmp", prefix="lunacy-e2e."))
        self.port = free_port()
        self.cdp_port = free_port()
        self.act_port = free_port()
        self.daemon: subprocess.Popen | None = None
        self.chrome: subprocess.Popen | None = None
        self.run_idx = 0

    def cleanup(self) -> None:
        if self.chrome and self.chrome.poll() is None:
            try:
                os.killpg(self.chrome.pid, signal.SIGTERM)
                self.chrome.wait(timeout=3)
            except Exception:
                try:
                    os.killpg(self.chrome.pid, signal.SIGKILL)
                except Exception:
                    pass
        if self.daemon and self.daemon.poll() is None:
            try:
                os.killpg(self.daemon.pid, signal.SIGTERM)
                self.daemon.wait(timeout=2)
            except Exception:
                try:
                    os.killpg(self.daemon.pid, signal.SIGKILL)
                    self.daemon.wait(timeout=2)
                except Exception:
                    pass
        shutil.rmtree(self.dir, ignore_errors=True)

    def env(self) -> dict[str, str]:
        e = dict(os.environ)
        e["TMPDIR"] = "/tmp"
        e["RUNE_DIRS_ROOT"] = str(self.dir / "rune-dirs")
        lvp_icd = "/home/tretrauit/Android/Sdk/emulator/lib64/vulkan/lvp_icd.json"
        if not e.get("VK_ICD_FILENAMES") and pathlib.Path(lvp_icd).is_file():
            e["VK_ICD_FILENAMES"] = lvp_icd
        return e

    def setup(self) -> None:
        assert self.port != 7878
        (self.dir / "bare").mkdir()
        (self.dir / "repos").mkdir()
        (self.dir / "state").mkdir()
        (self.dir / "rune-dirs").mkdir()

        ci_yml = (
            "name: CI\n"
            "on: [push]\n"
            "jobs:\n"
            "  web:\n"
            "    name: Web Check\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - name: Say hi\n"
            "        run: echo hello from act web\n"
            "      - name: Second check\n"
            "        run: echo act step 2\n"
        )
        release_yml = (
            "name: Release\n"
            "on: [push]\n"
            "jobs:\n"
            "  pkg:\n"
            "    name: Package\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - name: Pack\n"
            "        run: echo packaged\n"
        )
        git_init_repo(
            self.dir / "bare" / "alpha.git",
            self.dir / "repos" / "alpha",
            {
                ".github/workflows/ci.yml": ci_yml,
                ".github/workflows/release.yml": release_yml,
                "README.md": "alpha\n",
            },
            "feat: initial alpha",
        )
        git_init_repo(
            self.dir / "bare" / "beta.git",
            self.dir / "repos" / "beta",
            {".github/workflows/ci.yml": ci_yml, "README.md": "beta\n"},
            "feat: initial beta",
        )
        git_init_repo(
            self.dir / "bare" / "slow.git",
            self.dir / "repos" / "slow",
            {".github/workflows/ci.yml": ci_yml, "README.md": "slow\n"},
            "feat: initial slow",
        )

        self.web_dist = self.dir / "web_dist"
        print(f"[e2e] Building dev WASM bundle into {self.web_dist}...")
        web_build = RUNE_DIR / "scripts" / "web-build.sh"
        run_cmd(
            [str(web_build), "lunacy-ui", "--out", str(self.web_dist), "--dev"],
            cwd=ROOT / "ui",
            env=self.env(),
        )
        shutil.copyfile(ROOT / "ui" / "web" / "index.html", self.web_dist / "index.html")

        self.config_path = self.dir / "config.toml"
        self.write_initial_config()
        run_cmd([str(DAEMON_BIN), "check", "-c", str(self.config_path)], env=self.env())

        log_f = open(self.dir / "daemon.log", "w")
        self.daemon = subprocess.Popen(
            [str(DAEMON_BIN), "run", "-c", str(self.config_path)],
            stdout=log_f,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            env=self.env(),
        )
        wait_until(lambda: api_call(self.port, "/api/health", token=None)[0] == 200, timeout=15, desc="/api/health")
        print(f"[e2e] Sandbox daemon ready at http://127.0.0.1:{self.port}")

    def write_initial_config(self) -> None:
        cfg = f'''[general]
poll_interval = 300
state_dir = "{self.dir / "state"}"
log_level = "info"

[server]
listen = "127.0.0.1:{self.port}"
token = "{TOKEN}"
web_root = "{self.web_dist}"

[github]
webhook_secret = "{WEBHOOK_SECRET}"

[act]
command = ["act"]
args = ["--pull=false", "--artifact-server-port={self.act_port}"]
lock_file = "{self.dir / "act.lock"}"

[act.platforms]
"ubuntu-latest" = "-self-hosted"

[[project]]
name = "alpha"
repo_path = "{self.dir / "repos" / "alpha"}"
remote = "origin"
branch = "master"
checkout = "none"
poll_interval = 300

[project.env]
ALPHA_ENV = "hello"

[[project.ci]]
name = "Shell CI"
kind = "shell"
run = "seq 1 2500"
timeout = 60

[[project.ci]]
name = "act · CI"
kind = "act"
workflow = ".github/workflows/ci.yml"
job = "web"
event = "push"

[project.ci.secrets]
MY_SECRET = "alpha-secret"

[[project.ci]]
name = "Mac only"
kind = "shell"
run = "echo mac"
only_on = ["macos"]

[[project.deploy]]
name = "Deploy step"
kind = "shell"
run = "echo deployed $ALPHA_ENV"
timeout = 60

[[project]]
name = "beta"
repo_path = "{self.dir / "repos" / "beta"}"
remote = "origin"
branch = "master"
checkout = "none"
poll_interval = 300

[[project.ci]]
name = "Failing step"
kind = "shell"
run = "echo failing-line; exit 1"
timeout = 60

[[project]]
name = "slow"
repo_path = "{self.dir / "repos" / "slow"}"
remote = "origin"
branch = "master"
checkout = "none"
poll_interval = 300

[[project.ci]]
name = "Slow step"
kind = "shell"
run = "echo slow-start; sleep 25; echo slow-done"
timeout = 60
'''
        self.config_path.write_text(cfg)

    def run_ui(
        self,
        name: str,
        events: list[dict],
        *,
        at: float,
        token: str = TOKEN,
        size: str = "1280x840",
        fps: int = 20,
        scale: float | None = 1.0,
        snap: pathlib.Path | None = None,
        on_mark=None,
    ) -> dict:
        self.run_idx += 1
        prefix = f"{self.run_idx:02d}_{name}"
        script_path = self.dir / f"{prefix}.script.json"
        tree_path = self.dir / f"{prefix}.tree.json"
        script_path.write_text(json.dumps(events, indent=2))
        cmd = [
            str(UI_BIN),
            "--server",
            f"http://127.0.0.1:{self.port}",
            "--token",
            token,
            "--realtime",
            "--fps",
            str(fps),
            "--size",
            size,
            "--script",
            str(script_path),
            "--tree",
            str(tree_path),
            "--at",
            str(at),
            "--settle",
        ]
        if scale is not None:
            cmd.extend(["--scale", str(scale)])
        if snap is not None:
            cmd.extend(["--snap", str(snap)])
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            env=self.env(),
        )
        output_lines: list[str] = []
        assert proc.stdout is not None
        for line in proc.stdout:
            output_lines.append(line)
            if on_mark and line.startswith("rune-mark "):
                parts = line.strip().split()
                if len(parts) >= 2:
                    on_mark(parts[1])
        rc = proc.wait(timeout=60)
        if rc != 0:
            raise AssertionError(
                f"lunacy-ui ({name}) failed rc={rc}:\n{''.join(output_lines)}"
            )
        tree = json.loads(tree_path.read_text())
        warnings = tree.get("warnings", [])
        assert not warnings, f"{name}: tree warnings present: {warnings}"
        return tree


def wait_project_idle(port: int, name: str, timeout: float = 30.0) -> dict:
    def check():
        st, p = api_call(port, f"/api/projects/{name}")
        if st == 200 and isinstance(p, dict) and not p.get("running") and p.get("last_run"):
            return p
        return None

    return wait_until(check, timeout=timeout, desc=f"project {name} idle with last_run")


def test_contract(sb: Sandbox) -> str:
    print("[e2e] Checking daemon HTTP contract...")
    wait_project_idle(sb.port, "alpha")
    wait_project_idle(sb.port, "beta")

    st, health = api_call(sb.port, "/api/health", token=None)
    assert st == 200 and isinstance(health, dict) and health.get("ok") is True

    st401, _ = api_call(sb.port, "/api/projects", token=None)
    assert st401 == 401

    st, projects = api_call(sb.port, "/api/projects")
    assert st == 200 and isinstance(projects, list)
    assert [p["name"] for p in projects] == ["alpha", "beta", "slow"]

    st, alpha_p = api_call(sb.port, "/api/projects/alpha")
    assert st == 200 and alpha_p["name"] == "alpha"

    st, runs = api_call(sb.port, "/api/projects/alpha/runs?limit=50")
    assert st == 200 and isinstance(runs, list) and len(runs) >= 1
    alpha_run_id = runs[0]["id"]

    st, run = api_call(sb.port, f"/api/runs/{alpha_run_id}")
    if st != 200 or run["status"] != "success":
        _, act_err = api_call(sb.port, f"/api/runs/{alpha_run_id}/steps/1/log")
        raise AssertionError(
            f"alpha run status={run.get('status')} failed_step={run.get('failed_step')} "
            f"steps={run.get('steps')}\nstep1 log:\n{act_err}"
        )
    assert [s["name"] for s in run["steps"]] == ["Shell CI", "act · CI", "Mac only", "Deploy step"]
    assert run["steps"][1]["substeps"], "act step must record substeps"
    assert run["steps"][2]["status"] == "skipped"

    st, tail_log = api_call(sb.port, f"/api/runs/{alpha_run_id}/steps/0/log?tail=2000")
    assert st == 200 and isinstance(tail_log, str)
    tail_lines = tail_log.splitlines()
    assert len(tail_lines) == 2000 and tail_lines[0] == "501" and tail_lines[-1] == "2500"

    st, full_log = api_call(sb.port, f"/api/runs/{alpha_run_id}/steps/0/log")
    assert st == 200 and isinstance(full_log, str)
    full_lines = full_log.splitlines()
    assert len(full_lines) == 2500 and full_lines[0] == "1" and full_lines[-1] == "2500"

    st, act_log = api_call(sb.port, f"/api/runs/{alpha_run_id}/steps/1/log")
    assert st == 200 and "hello from act web" in str(act_log)

    beta = wait_project_idle(sb.port, "beta")
    assert beta["last_run"]["status"] == "failure"
    assert beta["last_run"]["failed_step"] == "Failing step"

    st, cfg = api_call(sb.port, "/api/config")
    assert st == 200 and isinstance(cfg, dict)
    assert cfg["server"]["token"] == "********"
    assert cfg["project"][0]["ci"][1]["secrets"]["MY_SECRET"] == "********"
    assert "log_level" in cfg["general"]
    assert "contexts" in cfg["project"][0]

    st, sysinfo = api_call(sb.port, "/api/system")
    assert st == 200 and isinstance(sysinfo, dict)
    assert sysinfo.get("state_dir") and sysinfo.get("config_path")
    wh = sysinfo.get("webhook")
    assert isinstance(wh, dict) and wh.get("configured") is True, sysinfo

    # Verify signed GitHub webhook ping updates GET /api/system webhook status.
    ping_body = json.dumps({"zen": "keep it logically awesome"}).encode()
    sig = "sha256=" + hmac.new(WEBHOOK_SECRET.encode(), ping_body, hashlib.sha256).hexdigest()
    ping_req = urllib.request.Request(
        f"http://127.0.0.1:{sb.port}/api/github/webhook",
        data=ping_body,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "X-GitHub-Event": "ping",
            "X-GitHub-Delivery": "e2e-ping-1",
            "X-Hub-Signature-256": sig,
        },
    )
    with urllib.request.urlopen(ping_req, timeout=5) as resp:
        assert resp.status == 200
        ping_res = json.loads(resp.read().decode("utf-8"))
        assert ping_res.get("ok") is True and ping_res.get("pong") is True, ping_res
    st, sysinfo_after = api_call(sb.port, "/api/system")
    assert st == 200 and isinstance(sysinfo_after, dict)
    wh_after = sysinfo_after.get("webhook") or {}
    assert wh_after.get("deliveries", 0) >= 1 and wh_after.get("last_event") == "ping", wh_after

    # Verify dead/orphaned run cancellation reconciles cleanly via API.
    dead_id = "alpha-orphaned-dead"
    dead_dir = sb.dir / "state" / "runs" / "alpha"
    dead_dir.mkdir(parents=True, exist_ok=True)
    (dead_dir / f"{dead_id}.json").write_text(
        json.dumps(
            {
                "id": dead_id,
                "project": "alpha",
                "sha": "0000000000000000000000000000000000000000",
                "subject": "orphaned dead run",
                "status": "running",
                "started_at": int(time.time()) - 60,
                "finished_at": None,
                "failed_step": None,
                "github_url": None,
                "steps": [
                    {
                        "phase": "ci",
                        "name": "Stuck step",
                        "status": "running",
                        "kind": "shell",
                        "substeps": [],
                    }
                ],
            }
        )
    )
    st, dead_cancel = api_call(sb.port, f"/api/runs/{dead_id}/cancel", method="POST", json_body={})
    assert st == 202 and isinstance(dead_cancel, dict), (st, dead_cancel)
    assert dead_cancel.get("orphaned") is True and dead_cancel.get("cancelled") is True, dead_cancel
    st, dead_run = api_call(sb.port, f"/api/runs/{dead_id}")
    assert st == 200 and isinstance(dead_run, dict) and dead_run.get("status") == "cancelled", dead_run

    st, deploy_checks = api_call(sb.port, "/api/deploy/checks")
    assert st == 200 and isinstance(deploy_checks, dict)
    assert deploy_checks.get("os") and isinstance(deploy_checks.get("projects"), list)
    assert [p["name"] for p in deploy_checks["projects"]] == ["alpha", "beta", "slow"]

    st, wf1 = api_call(sb.port, "/api/projects/alpha/workflows")
    assert st == 200 and isinstance(wf1, list)
    assert {w["file"] for w in wf1} == {".github/workflows/ci.yml", ".github/workflows/release.yml"}

    repo_q = urllib.parse.quote(str(sb.dir / "repos" / "alpha"), safe="")
    st, wf2 = api_call(sb.port, f"/api/projects/_/workflows?repo_path={repo_q}")
    assert st == 200 and len(wf2) == 2
    return alpha_run_id


def test_native_flows(sb: Sandbox, alpha_run_id: str) -> None:
    print("[e2e] T1: Unauthorized banner -> Connection fix -> Projects...")
    t_unauth = sb.run_ui("unauth", [], at=0.8, token="wrong-token")
    assert "Unauthorized" in " ".join(subtree_texts(t_unauth, "link-banner"))

    sc = Script(1.0).tap("banner-connection", 0.55).replace("conn-token", TOKEN, 0.40).tap("conn-connect", 1.1).tap("nav-Projects", 0.6)
    t_recon = sb.run_ui("reconnect", sc.events, at=sc.t + 0.5, token="wrong-token")
    assert not has_key(t_recon, "link-banner"), "banner should clear after valid reconnect"
    assert "Connected" in " ".join(subtree_texts(t_recon, "connection"))
    for p in ("project-alpha", "project-beta", "project-slow"):
        assert has_key(t_recon, p), f"missing project card {p}"

    print("[e2e] T2: Force trigger -> alpha new run + toast dismiss...")
    wait_project_idle(sb.port, "alpha")
    before_runs = api_call(sb.port, "/api/projects/alpha/runs")[1]
    assert isinstance(before_runs, list)
    n_before = len(before_runs)

    sc = Script(1.0).tap("projects-force", 0.25).tap("run-now-alpha", 0.6)
    t_trig = sb.run_ui("force_trigger", sc.events, at=sc.t + 0.3)
    toast_txt = " ".join(subtree_texts(t_trig, "toast-layer"))
    assert "Queued" in toast_txt and "alpha: a run was queued." in toast_txt, toast_txt
    wait_until(
        lambda: len(api_call(sb.port, "/api/projects/alpha/runs")[1]) > n_before,
        timeout=15,
        desc="manual forced run created for alpha",
    )
    alpha_p = wait_project_idle(sb.port, "alpha")
    latest_alpha_run_id = alpha_p["last_run"]["id"]
    assert alpha_p["last_run"]["trigger"] == "manual"

    sc = Script(1.0).tap("projects-force", 0.25).tap("run-now-alpha", 0.7).tap("toast-close-1", 0.3)
    t_dismiss = sb.run_ui("toast_dismiss", sc.events, at=sc.t + 0.3)
    assert not has_key(t_dismiss, "toast-1"), "dismissed toast must vanish"
    wait_project_idle(sb.port, "alpha")

    print("[e2e] T3: Live WebSocket push update, 409 conflict & live slow run log...")
    wait_project_idle(sb.port, "slow", timeout=40.0)

    def on_mark_slow(name: str) -> None:
        if name == "fire-slow":
            st, _ = api_call(sb.port, "/api/projects/slow/trigger", method="POST", json_body={"force": True})
            assert st == 202

    # External trigger pushes `projects_updated` over WebSocket within milliseconds (well before
    # any 2s HTTP poll interval), transitioning `slow` to running and surfacing `cancel-slow`.
    sc = Script(1.0).mark("fire-slow", 0.45)
    t_ws_running = sb.run_ui("slow_ws_running", sc.events, at=sc.t + 0.15, on_mark=on_mark_slow)
    assert has_key(t_ws_running, "cancel-slow"), (
        "expected WebSocket push to surface cancel-slow within 0.6s of external trigger"
    )
    run_now_slow = find_el(t_ws_running, "run-now-slow")
    assert (
        run_now_slow.get("disabled") is True
        or run_now_slow.get("help") == "Already running"
    ), f"expected run-now-slow disabled once running, got {run_now_slow}"

    def slow_running_project() -> dict | None:
        st, p = api_call(sb.port, "/api/projects/slow")
        if (
            st == 200
            and isinstance(p, dict)
            and p.get("running") is True
            and isinstance(p.get("current_run"), dict)
            and p["current_run"].get("id")
        ):
            return p
        return None

    slow_p = wait_until(slow_running_project, timeout=10, desc="slow marked running")
    st_409, body_409 = api_call(sb.port, "/api/projects/slow/trigger", method="POST", json_body={"force": True})
    assert st_409 == 409, f"expected 409 while slow is running, got {st_409}: {body_409}"

    slow_id = slow_p["current_run"]["id"]
    sc = Script(1.0).tap("project-slow", 0.7).tap(f"run-{slow_id}", 0.7).tap("step-0", 0.7)
    t_slow_live = sb.run_ui("slow_live", sc.events, at=sc.t + 0.3)
    slow_log_text = " ".join(subtree_texts(t_slow_live, "log"))
    assert "slow-start" in slow_log_text, f"expected slow-start in live log, got: {slow_log_text[:200]}"
    assert has_key(t_slow_live, "run-cancel"), "Cancel button must appear on running run"

    print("[e2e] T3b: Cancel running slow workflow from run screen...")
    st_slow, slow_p = api_call(sb.port, "/api/projects/slow")
    cur_run = slow_p.get("current_run") if st_slow == 200 and isinstance(slow_p, dict) else None
    remaining = (
        25.0 - (time.time() - float(cur_run["started_at"]))
        if isinstance(cur_run, dict) and cur_run.get("started_at") is not None
        else 0.0
    )
    if not (
        isinstance(slow_p, dict)
        and slow_p.get("running") is True
        and isinstance(cur_run, dict)
        and cur_run.get("id")
        and remaining >= 12.0
    ):
        if isinstance(slow_p, dict) and slow_p.get("running"):
            wait_project_idle(sb.port, "slow", timeout=40.0)
        st_trig, _ = api_call(
            sb.port,
            "/api/projects/slow/trigger",
            method="POST",
            json_body={"force": True},
        )
        assert st_trig == 202, f"expected 202 triggering fresh slow run, got {st_trig}"
        slow_p = wait_until(slow_running_project, timeout=10, desc="fresh slow run marked running")
        slow_id = slow_p["current_run"]["id"]
    else:
        slow_id = cur_run["id"]

    sc_cancel = Script(1.0).tap("project-slow", 0.6).tap(f"run-{slow_id}", 0.6).tap("run-cancel", 0.8)
    t_cancel = sb.run_ui("slow_cancel", sc_cancel.events, at=sc_cancel.t + 0.3)
    assert "Cancelling" in " ".join(subtree_texts(t_cancel, "toast-layer"))
    slow_done = wait_project_idle(sb.port, "slow")
    assert slow_done["last_run"]["id"] == slow_id
    assert slow_done["last_run"]["status"] == "cancelled", slow_done["last_run"]

    print("[e2e] T4: History -> Run -> Tail/Full log -> act substeps -> skipped -> Back/Esc/Cmd+R...")
    sc = (
        Script(1.0)
        .tap("project-alpha", 0.7)
        .tap(f"run-{latest_alpha_run_id}", 0.7)
        .tap("step-0", 0.7)
        .tap("run-log-mode/segment1", 0.7)
    )
    t_full = sb.run_ui("alpha_run_full", sc.events, at=sc.t + 0.3)
    full_txt = " ".join(subtree_texts(t_full, "log"))
    assert "1\n2\n" in full_txt and "2500" in full_txt, "Full toggle did not load line 1..2500"

    sc = (
        Script(1.0)
        .tap("project-alpha", 0.7)
        .tap(f"run-{latest_alpha_run_id}", 0.7)
        .tap("step-1", 0.7)
        .tap("substep-toggle-0", 0.4)
    )
    t_act = sb.run_ui("alpha_act_and_skipped", sc.events, at=sc.t + 0.3)
    subs = " ".join(subtree_texts(t_act, "substeps-job-0"))
    assert "Say hi" in subs and "Second check" in subs, subs
    assert "hello from act web" in " ".join(subtree_texts(t_act, "substeps")), subs
    assert "Skipped" in " ".join(subtree_texts(t_act, "step-2"))

    sc_raw = (
        Script(1.0)
        .tap("project-alpha", 0.7)
        .tap(f"run-{latest_alpha_run_id}", 0.7)
        .tap("step-1", 0.7)
        .tap("run-log-view/segment1", 0.4)
    )
    t_act_raw = sb.run_ui("alpha_act_raw", sc_raw.events, at=sc_raw.t + 0.3)
    assert "hello from act web" in " ".join(subtree_texts(t_act_raw, "log"))

    sc = (
        Script(1.0)
        .tap("project-alpha", 0.6)
        .tap(f"run-{latest_alpha_run_id}", 0.6)
        .tap("run-back", 0.5)
        .tap("project-back", 0.5)
        .tap("project-beta", 0.5)
        .key("Escape", after=0.5)
        .key("r", cmd=True, after=0.8)
    )
    t_nav = sb.run_ui("nav_back_esc_refresh", sc.events, at=sc.t + 0.3)
    assert has_key(t_nav, "projects-header")
    assert "Connected" in " ".join(subtree_texts(t_nav, "connection"))

    print("[e2e] T5: Settings pages, 400 validation, Discard, Cmd+S Save, Diagnostics, masked secrets...")
    sc = Script(1.0).key(",", cmd=True, after=0.7).replace("field-general.poll_interval", "", 0.3).tap("settings-save", 0.7)
    t_bad = sb.run_ui("settings_400", sc.events, at=sc.t + 0.3)
    bad_page = " ".join(subtree_texts(t_bad, "config-page"))
    assert "general.poll_interval" in bad_page and "at least 1 second" in bad_page, bad_page

    sc = Script(1.0).tap("titlebar-settings", 0.7).replace("field-general.poll_interval", "45", 0.3).tap("settings-discard", 0.4)
    t_disc = sb.run_ui("settings_discard", sc.events, at=sc.t + 0.3)
    assert "Unsaved" not in " ".join(subtree_texts(t_disc, "settings-bar"))
    assert "300" in subtree_texts(t_disc, "field-general.poll_interval/control")

    sc = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .replace("field-general.poll_interval", "120", 0.25)
        .replace("field-general.step_timeout", "1800", 0.25)
        .replace("field-general.shell", "bash -lc", 0.25)
        .tap("settings-nav-Server", 0.35)
        .replace("field-server.public_url", f"http://127.0.0.1:{sb.port}", 0.25)
        .tap("settings-nav-GitHub", 0.35)
        .tap("github-mode/segment1", 0.25)
        .replace("field-github.token", "ghp_e2e_test", 0.25)
        .tap("settings-nav-Discord", 0.35)
        .tap("discord-notify-on-start", 0.25)
        .tap("settings-nav-act", 0.35)
        .replace("field-act.args", f"--pull=false\n--artifact-server-port={sb.act_port}\n--verbose", 0.25)
        .replace("field-act.platforms", "ubuntu-latest=-self-hosted\nubuntu-22.04=-self-hosted", 0.25)
        .key("s", cmd=True, after=0.9)
        .tap("settings-nav-Deploy", 0.6)
        .tap("deploy-refresh", 0.6)
    )
    t_deploy = sb.run_ui("settings_save_and_deploy", sc.events, at=sc.t + 0.4)
    dep_txt = " ".join(subtree_texts(t_deploy, "deploy-checks"))
    assert "alpha" in dep_txt and "1800s" in dep_txt, dep_txt
    assert has_key(t_deploy, "deploy-project-alpha")

    sc_diag = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .tap("settings-nav-Diagnostics", 0.5)
        .tap("diagnostics-refresh", 0.6)
    )
    t_save = sb.run_ui("settings_diagnostics", sc_diag.events, at=sc_diag.t + 0.4)
    diag = " ".join(subtree_texts(t_save, "diagnostics"))
    assert "Linux" in diag and "Available" in diag and str(sb.config_path) in diag, diag
    assert str(sb.dir / "state") in diag, diag

    disk = tomllib.loads(sb.config_path.read_text())
    assert disk["general"]["poll_interval"] == 120
    assert disk["general"]["step_timeout"] == 1800
    assert disk["general"]["shell"] == ["bash", "-lc"]
    assert disk["server"]["token"] == TOKEN, "masked server.token was clobbered!"
    assert disk["server"]["public_url"] == f"http://127.0.0.1:{sb.port}"
    assert disk["github"]["mode"] == "status"
    assert disk["github"]["token"] == "ghp_e2e_test"
    assert disk["github"]["webhook_secret"] == WEBHOOK_SECRET, "masked github.webhook_secret was clobbered!"
    assert disk["discord"]["notify_on_start"] is True
    assert disk["act"]["args"] == ["--pull=false", f"--artifact-server-port={sb.act_port}", "--verbose"]
    assert disk["act"]["platforms"]["ubuntu-22.04"] == "-self-hosted"
    assert disk["project"][0]["ci"][1]["secrets"]["MY_SECRET"] == "alpha-secret", "masked secret lost!"
    run_cmd([str(DAEMON_BIN), "check", "-c", str(sb.config_path)], env=sb.env())

    print("[e2e] T6: Settings -> Projects & Steps CRUD, workflow picker, validation, worker reconcile...")
    sc = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .tap("settings-nav-Projects", 0.4)
        .tap("project-edit-0", 0.4)
        .replace("field-project[0].github_repo", "acme/alpha", 0.25)
        .replace("field-project[0].poll_interval", "90", 0.25)
        .replace("field-project[0].step_timeout", "600", 0.25)
        .replace("field-project[0].timeout", "3600", 0.25)
        .wheel("settings-body", -350.0, 0.3)
        .replace("field-project[0].env", "ALPHA_ENV=hello\nFOO=bar", 0.25)
        .tap("settings-save", 0.8)
    )
    sb.run_ui("project_edit_alpha", sc.events, at=sc.t + 0.4)
    disk = tomllib.loads(sb.config_path.read_text())
    assert disk["project"][0]["github_repo"] == "acme/alpha"
    assert disk["project"][0]["poll_interval"] == 90
    assert disk["project"][0]["step_timeout"] == 600
    assert disk["project"][0]["timeout"] == 3600
    assert disk["project"][0]["env"]["FOO"] == "bar"

    sc = Script(1.0).tap("project-alpha", 0.6).tap(f"run-{latest_alpha_run_id}", 0.6).tap("run-github", 0.3)
    t_gh = sb.run_ui("run_view_github_button", sc.events, at=sc.t + 0.3)
    assert has_key(t_gh, "run-github"), "View on GitHub button must appear when github_repo is set"

    sc = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .tap("settings-nav-Projects", 0.4)
        .tap("project-down-0", 0.3)
        .tap("project-up-1", 0.3)
        .tap("project-edit-0", 0.4)
        .tap("project-duplicate", 0.4)
        .tap("settings-save", 0.8)
    )
    sb.run_ui("project_reorder_duplicate", sc.events, at=sc.t + 0.4)
    disk = tomllib.loads(sb.config_path.read_text())
    assert [p["name"] for p in disk["project"]] == ["alpha", "beta", "slow", "alpha-copy"]
    assert disk["project"][3]["ci"][1]["secrets"]["MY_SECRET"] != "********"

    sc = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .tap("settings-nav-Projects", 0.4)
        .tap("project-edit-3", 0.4)
        .tap("project-delete", 0.3)
        .tap("project-delete-cancel", 0.3)
        .tap("project-delete", 0.3)
        .tap("project-delete-confirm", 0.3)
        .tap("settings-save", 0.8)
    )
    sb.run_ui("project_delete_copy", sc.events, at=sc.t + 0.4)
    projs_after_del = [p["name"] for p in api_call(sb.port, "/api/projects")[1]]
    assert projs_after_del == ["alpha", "beta", "slow"], projs_after_del

    sc = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .tap("settings-nav-Projects", 0.4)
        .tap("projects-add", 0.4)
        .replace("field-project[3].name", "delta", 0.25)
        .replace("field-project[3].branch", "master", 0.25)
        .replace("field-project[3].repo_path", str(sb.dir / "repos" / "alpha"), 0.25)
        .wheel("settings-body", -800.0, 0.3)
        .tap("add-shell-ci", 0.4)
        .wheel("settings-body", -450.0, 0.3)
        .replace("field-project[3].ci[0].name", "Smoke", 0.25)
        .replace("field-project[3].ci[0].run", "", 0.25)
        .tap("only-on-linux", 0.25)
        .wheel("settings-body", -300.0, 0.3)
        .replace("field-project[3].ci[0].timeout", "30", 0.25)
        .replace("field-project[3].ci[0].shell", "bash -lc", 0.25)
        .tap("settings-save", 0.8)
    )
    t_step_bad = sb.run_ui("add_project_and_steps_400", sc.events, at=sc.t + 0.4)
    assert "run" in " ".join(subtree_texts(t_step_bad, "project-form"))

    sc = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .tap("settings-nav-Projects", 0.4)
        .tap("projects-add", 0.4)
        .replace("field-project[3].name", "delta", 0.25)
        .replace("field-project[3].branch", "master", 0.25)
        .replace("field-project[3].repo_path", str(sb.dir / "repos" / "alpha"), 0.25)
        .wheel("settings-body", -800.0, 0.3)
        .tap("add-shell-ci", 0.4)
        .wheel("settings-body", -350.0, 0.3)
        .replace("field-project[3].ci[0].name", "Smoke", 0.25)
        .replace("field-project[3].ci[0].run", "echo smoke", 0.25)
        .tap("step-done", 0.3)
        .tap("add-act-ci", 0.9)
        .wheel("settings-body", -350.0, 0.3)
        .tap("step-workflows/choice-.github/workflows/release.yml", 0.3)
        .tap("step-kind/segment0", 0.25)
        .tap("step-kind/segment1", 0.25)
        .tap("step-done", 0.3)
        .tap("step-down-ci-0", 0.3)
        .tap("step-up-ci-1", 0.3)
        .tap("step-remove-ci-1", 0.3)
        .tap("settings-save", 0.8)
    )
    sb.run_ui("add_project_act_workflows_and_save", sc.events, at=sc.t + 0.4)
    projs_with_delta = [p["name"] for p in api_call(sb.port, "/api/projects")[1]]
    assert "delta" in projs_with_delta, projs_with_delta

    sc = (
        Script(1.0)
        .tap("titlebar-settings", 0.7)
        .tap("settings-nav-Projects", 0.4)
        .tap("project-edit-3", 0.4)
        .tap("project-delete", 0.3)
        .tap("project-delete-confirm", 0.3)
        .tap("settings-save", 0.8)
    )
    sb.run_ui("delete_delta", sc.events, at=sc.t + 0.4)
    wait_until(
        lambda: [p["name"] for p in api_call(sb.port, "/api/projects")[1]] == ["alpha", "beta", "slow"],
        timeout=5,
        desc="delta worker removed",
    )


def assert_tree_responsive(
    label: str,
    tree: dict,
    *,
    win_w: float,
    expect_compact: bool,
    expect_zoom: float,
    extra_scroll_keys: tuple[str, ...] = ("settings-nav-strip",),
) -> None:
    vp = tree.get("viewport") or [0.0, 0.0]
    vw = float(vp[0])
    assert vw > 0.0, f"{label}: missing viewport {vp}"
    actual_zoom = win_w / vw
    assert abs(actual_zoom - expect_zoom) < 0.02, (
        f"{label} @ {win_w}: expected zoom {expect_zoom}, got {actual_zoom:.3f} (vp={vp})"
    )
    assert has_key(tree, "titlebar-menu") == expect_compact, (
        f"{label} @ {win_w}: titlebar-menu mismatch (expect_compact={expect_compact})"
    )
    assert has_key(tree, "rail") == (not expect_compact), (
        f"{label} @ {win_w}: rail mismatch (expect_compact={expect_compact})"
    )

    nodes = elems(tree)
    by_path = {str(x.get("path", "")): x for x in nodes if x.get("path")}
    scroll_strip_paths = [
        str(s.get("path", ""))
        for sk in extra_scroll_keys
        for s in find_els(tree, sk)
        if s.get("path")
    ]
    interactive_roles = {"button", "tab", "checkbox", "radio", "switch", "textbox"}
    for n in nodes:
        if n.get("exiting") or n.get("inert"):
            continue
        x, _y, w, h = el_rect(n)
        role = n.get("role", "")
        key = n.get("key")
        path = str(n.get("path", ""))
        in_hscroll = any(path == sp or path.startswith(sp + "/") for sp in scroll_strip_paths)
        interactive = bool(n.get("focusable")) or role in interactive_roles
        if interactive and not in_hscroll:
            assert x >= -0.5 and x + w <= vw + 0.5, (
                f"{label} @ {win_w}: interactive overflow key={key!r} role={role!r} rect={[x, _y, w, h]} > vw={vw:.1f}"
            )
        if expect_compact and interactive and role != "link" and key != "drawer-scrim":
            if role == "textbox" and key == "input" and "/" in path:
                parent = by_path.get(path.rsplit("/", 1)[0])
                tap_h = el_rect(parent)[3] if parent is not None else h
            else:
                tap_h = h
            assert tap_h >= 43.9, (
                f"{label} @ {win_w}: touch target < 44pt key={key!r} role={role!r} path={path!r} tap_h={tap_h:.1f}"
            )
        if key in extra_scroll_keys:
            assert x >= -0.5 and x + w <= vw + 0.5, (
                f"{label} @ {win_w}: scroll strip {key!r} overflows vw={vw:.1f}: {[x, _y, w, h]}"
            )
        txt = (n.get("text") or "").strip()
        if expect_compact and txt and key != "titlebar-title" and not in_hscroll:
            assert x >= -0.5 and x + w <= vw + 0.5, (
                f"{label} @ {win_w}: text overflows vw={vw:.1f} key={key!r} text={txt!r} rect={[x, _y, w, h]}"
            )


def test_responsive_layouts(sb: Sandbox, alpha_run_id: str) -> None:
    print("[e2e] Testing responsive layouts (390x844, 360x780, 768x1024, 1180x760)...")
    profiles = [
        ("390x844", 390.0, True, 1.0),
        ("360x780", 360.0, True, 1.0),
        ("768x1024", 768.0, True, 1.1),
        ("1180x760", 1180.0, False, 1.1),
    ]
    for size, win_w, compact, zoom in profiles:
        tag = size.replace("x", "_")

        t_proj_list = sb.run_ui(f"resp_{tag}_projects", [], at=1.2, size=size)
        assert has_key(t_proj_list, "project-alpha")
        assert_tree_responsive("projects", t_proj_list, win_w=win_w, expect_compact=compact, expect_zoom=zoom)

        sc = Script(1.0).tap("project-alpha", 0.7)
        t_proj = sb.run_ui(f"resp_{tag}_project", sc.events, at=sc.t + 0.3, size=size)
        assert has_key(t_proj, f"run-{alpha_run_id}")
        assert_tree_responsive("project", t_proj, win_w=win_w, expect_compact=compact, expect_zoom=zoom)

        sc = Script(1.0).tap("project-alpha", 0.7).tap(f"run-{alpha_run_id}", 0.7).tap("step-0", 0.7)
        t_run = sb.run_ui(f"resp_{tag}_run", sc.events, at=sc.t + 0.4, size=size)
        assert has_key(t_run, "log") and has_key(t_run, "run-log-mode") and has_key(t_run, "run-log-wrap")
        assert_tree_responsive("run", t_run, win_w=win_w, expect_compact=compact, expect_zoom=zoom)

        sc = Script(1.0).tap("titlebar-settings", 0.7)
        t_gen = sb.run_ui(f"resp_{tag}_settings_general", sc.events, at=sc.t + 0.3, size=size)
        assert has_key(t_gen, "field-general.poll_interval")
        if compact:
            assert has_key(t_gen, "settings-nav-strip-scroll-area")
        else:
            assert has_key(t_gen, "settings-pages-scroll-scroll-area")
        assert_tree_responsive("settings-general", t_gen, win_w=win_w, expect_compact=compact, expect_zoom=zoom)

        sc = Script(1.0).tap("connection", 0.7)
        t_conn = sb.run_ui(f"resp_{tag}_settings_conn", sc.events, at=sc.t + 0.3, size=size)
        assert has_key(t_conn, "conn-server") and has_key(t_conn, "conn-token")
        assert_tree_responsive("settings-connection", t_conn, win_w=win_w, expect_compact=compact, expect_zoom=zoom)

        sc = Script(1.0).tap("titlebar-settings", 0.7)
        if win_w <= 420.0:
            sc = sc.wheel("settings-nav-strip", dx=-240.0, after=0.35)
        sc = sc.tap("settings-nav-Projects", 0.5).tap("project-edit-0", 0.4)
        if win_w <= 420.0:
            sc = sc.wheel("settings-body", -800.0, 0.35).wheel("settings-body", -800.0, 0.35)
        elif compact:
            sc = sc.wheel("settings-body", -900.0, 0.35).wheel("settings-body", -900.0, 0.35)
        else:
            sc = sc.wheel("settings-body", -950.0, 0.35)
        sc = sc.tap("step-row-ci-0", 0.4)
        t_step = sb.run_ui(f"resp_{tag}_step_editor", sc.events, at=sc.t + 0.3, size=size)
        assert has_key(t_step, "step-form") and has_key(t_step, "step-kind")
        assert_tree_responsive("settings-step-editor", t_step, win_w=win_w, expect_compact=compact, expect_zoom=zoom)

        t_unauth = sb.run_ui(f"resp_{tag}_unauth", [], at=1.0, token="wrong-token", size=size)
        assert "Unauthorized" in " ".join(subtree_texts(t_unauth, "link-banner"))
        assert_tree_responsive("unauthorized", t_unauth, win_w=win_w, expect_compact=compact, expect_zoom=zoom)

        if compact:
            sc = Script(1.0).tap("titlebar-menu", 0.4)
            t_drawer = sb.run_ui(f"resp_{tag}_drawer", sc.events, at=sc.t + 0.2, size=size)
            assert has_key(t_drawer, "rail-drawer") and has_key(t_drawer, "nav-Settings")
            assert_tree_responsive("drawer", t_drawer, win_w=win_w, expect_compact=True, expect_zoom=zoom)


def test_job_b_features(sb: Sandbox, alpha_run_id: str) -> None:
    print("[e2e] Testing Job B (log auto-follow, scrollbars, OLED theme + zoom persistence)...")
    t_home = sb.run_ui("jobb_home", [], at=1.1, size="1180x760")
    assert has_key(t_home, "projects-scroll-scroll-area")
    assert has_key(t_home, "rail-scroll-scroll-area")

    # B1 & B2: run log starts at bottom, keeps bottom across Tail -> Full switch, mounts scrollbars + thumb.
    sc = Script(1.0).tap("project-alpha", 0.7).tap(f"run-{alpha_run_id}", 0.7).tap("step-0", 0.8)
    t_tail = sb.run_ui("jobb_log_tail", sc.events, at=sc.t + 0.3, size="1180x760")
    assert has_key(t_tail, "run-steps-scroll-scroll-area")
    assert has_key(t_tail, "run-log-scroll-scroll-area")
    assert has_key(t_tail, "run-log-wrap")
    assert not has_key(t_tail, "scroll-bar-x"), "wrapped log must not mount horizontal scrollbar"
    assert has_key(t_tail, "thumb")
    vp = el_rect(find_el(t_tail, "run-log-scroll"))
    lg = el_rect(find_el(t_tail, "log"))
    assert abs((lg[1] + lg[3]) - (vp[1] + vp[3])) <= 2.5, f"Tail not at bottom: vp={vp} log={lg}"

    sc_full = sc.tap("run-log-mode/segment1", 0.8)
    t_full = sb.run_ui("jobb_log_full", sc_full.events, at=sc_full.t + 0.4, size="1180x760")
    vp_f = el_rect(find_el(t_full, "run-log-scroll"))
    lg_f = el_rect(find_el(t_full, "log"))
    assert abs((lg_f[1] + lg_f[3]) - (vp_f[1] + vp_f[3])) <= 2.5, f"Full not at bottom: vp={vp_f} log={lg_f}"

    # Toggle run-log-wrap off -> mounts horizontal scrollbar and persists across UI restart; then reset back to wrapped.
    sc_unwrap = Script(1.0).tap("project-alpha", 0.7).tap(f"run-{alpha_run_id}", 0.7).tap("step-0", 0.7).tap("run-log-wrap", 0.4)
    t_unwrap = sb.run_ui("jobb_log_unwrap", sc_unwrap.events, at=sc_unwrap.t + 0.3, size="1180x760")
    assert has_key(t_unwrap, "scroll-bar-x"), "unwrapped log must mount horizontal scrollbar"

    sc_unwrap_persist = Script(1.0).tap("project-alpha", 0.7).tap(f"run-{alpha_run_id}", 0.7).tap("step-0", 0.7)
    t_unwrap_persist = sb.run_ui("jobb_log_unwrap_persist", sc_unwrap_persist.events, at=sc_unwrap_persist.t + 0.3, size="1180x760")
    assert has_key(t_unwrap_persist, "scroll-bar-x"), "unwrapped log preference must persist across restart"

    sc_rewrap = Script(1.0).tap("project-alpha", 0.7).tap(f"run-{alpha_run_id}", 0.7).tap("step-0", 0.7).tap("run-log-wrap", 0.4)
    t_rewrap = sb.run_ui("jobb_log_rewrap", sc_rewrap.events, at=sc_rewrap.t + 0.3, size="1180x760")
    assert not has_key(t_rewrap, "scroll-bar-x"), "rewrapped log must unmount horizontal scrollbar"

    # B4 & B5: OLED + 1.25x zoom in Appearance, persisted across restart, then Ctrl/Cmd+= & Ctrl/Cmd+0.
    sc_set = (
        Script(1.0)
        .tap("titlebar-settings", 0.6)
        .tap("settings-nav-Appearance", 0.4)
        .tap("appearance-theme/choice-oled", 0.35)
        .tap("appearance-zoom/choice-1.25", 0.4)
    )
    t_oled = sb.run_ui("jobb_oled_125", sc_set.events, at=sc_set.t + 0.4, size="1180x760")
    assert find_el(t_oled, "app-root").get("fill") == "#000000ff"
    assert has_key(t_oled, "settings-body-scroll-area")
    assert has_key(t_oled, "settings-pages-scroll-scroll-area")
    assert_tree_responsive("appearance-oled-1.25", t_oled, win_w=1180.0, expect_compact=False, expect_zoom=1.25)

    # Fresh UI instance in the same RUNE_DIRS_ROOT restores OLED + 1.25x.
    t_persist = sb.run_ui("jobb_persist", [], at=1.2, size="1180x760")
    assert find_el(t_persist, "app-root").get("fill") == "#000000ff"
    assert_tree_responsive("persisted-oled-1.25", t_persist, win_w=1180.0, expect_compact=False, expect_zoom=1.25)

    # Ctrl/Cmd+= steps 1.25 -> 1.5; then reset cleanly to System + default 1.1x for subsequent steps.
    sc_15 = Script(1.0).key("=", cmd=True, after=0.4)
    t_15 = sb.run_ui("jobb_zoom_15", sc_15.events, at=sc_15.t + 0.3, size="1180x760")
    assert_tree_responsive("shortcut-1.5", t_15, win_w=1180.0, expect_compact=False, expect_zoom=1.5)

    sc_reset = (
        Script(1.0)
        .key("0", cmd=True, after=0.4)
        .tap("titlebar-settings", 0.6)
        .tap("settings-nav-Appearance", 0.4)
        .tap("appearance-theme/choice-system", 0.35)
    )
    t_rst = sb.run_ui("jobb_reset", sc_reset.events, at=sc_reset.t + 0.3, size="1180x760")
    assert find_el(t_rst, "app-root").get("fill") != "#000000ff"
    assert_tree_responsive("shortcut-reset-1.1", t_rst, win_w=1180.0, expect_compact=False, expect_zoom=1.1)


def test_web_build(sb: Sandbox, alpha_run_id: str) -> None:
    print("[e2e] Testing web build (WASM + fetch) in headless Chrome over CDP...")
    t_home = sb.run_ui("web_coords_home", [], at=1.1, size="1180x760")
    t_set = sb.run_ui(
        "web_coords_settings",
        Script(1.0).tap("titlebar-settings").events,
        at=1.6,
        size="1180x760",
    )
    coords = {
        "settings": el_center(find_el(t_home, "titlebar-settings"), t_home, 1180.0),
        "poll_input": el_center(find_el(t_set, "field-general.poll_interval/control"), t_set, 1180.0),
        "save": el_center(find_el(t_set, "settings-save"), t_set, 1180.0),
    }

    chrome_dir = sb.dir / "chrome"
    chrome_dir.mkdir(exist_ok=True)
    sb.chrome = subprocess.Popen(
        [
            "google-chrome-stable",
            "--headless=new",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--enable-unsafe-webgpu",
            "--use-gl=angle",
            "--use-angle=swiftshader",
            "--enable-features=Vulkan",
            "--window-size=1180,760",
            f"--remote-debugging-port={sb.cdp_port}",
            f"--user-data-dir={chrome_dir}",
            "about:blank",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
        env=sb.env(),
    )
    wait_until(
        lambda: isinstance(
            json.loads(urllib.request.urlopen(f"http://127.0.0.1:{sb.cdp_port}/json/list", timeout=1).read()),
            list,
        ),
        timeout=15,
        desc="Chrome CDP ready",
    )
    out = run_cmd(
        ["node", str(WEB_CHECK), str(sb.cdp_port), str(sb.port), TOKEN, alpha_run_id, json.dumps(coords)],
        env=sb.env(),
    )
    print(f"[e2e] Chrome CDP result: {out.strip()}")
    disk = tomllib.loads(sb.config_path.read_text())
    assert disk["general"]["poll_interval"] == 150, f"expected poll_interval=150 from web PUT, got {disk['general']['poll_interval']}"
    run_cmd([str(DAEMON_BIN), "check", "-c", str(sb.config_path)], env=sb.env())


def main() -> int:
    sb = Sandbox()
    try:
        sb.setup()
        alpha_run_id = test_contract(sb)
        test_native_flows(sb, alpha_run_id)
        test_responsive_layouts(sb, alpha_run_id)
        test_job_b_features(sb, alpha_run_id)
        test_web_build(sb, alpha_run_id)
        print("[e2e] ALL CHECKS PASSED!")
        return 0
    finally:
        sb.cleanup()


if __name__ == "__main__":
    sys.exit(main())
