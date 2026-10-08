"""Smoke test against the real `fm` on this Mac. Run it through real_fm_smoke.sh.

Sections (default: facts tools):
  facts     re-checks the AGENTS.md verified facts directly against `fm`
  tools     calls each tool through the fm-mcp binary
  sessions  several fm-mcp sessions at once, and a long summarise (about 4 min)

Standard library only. Exit code 1 if any check fails.
"""

import http.client
import json
import os
import random
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
FM = os.environ.get("FM_MCP_FM_PATH", "/usr/bin/fm")
BINARY = Path(os.environ.get("FM_MCP_BINARY", REPO / "target/release/fm-mcp"))
RENDER = Path(__file__).resolve().parent / "render_text.js"
SCRATCH = Path(tempfile.mkdtemp(prefix="fm-smoke-"))

results = []  # (status, name, detail)


def report(status, name, detail=""):
    results.append((status, name, detail))
    mark = {"PASS": "✓", "FAIL": "✗", "WARN": "!", "INFO": "·"}[status]
    print(f"  {mark} {name}" + (f": {detail}" if detail else ""), flush=True)


def check(ok, name, detail=""):
    report("PASS" if ok else "FAIL", name, detail)
    return ok


# ---------------------------------------------------------------- helpers


def first_line(text):
    """The first non-empty line, without terminal colours (`fm` errors are coloured)."""
    lines = [l for l in re.sub(r"\x1b\[[0-9;]*m", "", text).splitlines() if l.strip()]
    return lines[0].strip() if lines else ""


def count_tokens(text):
    out = subprocess.run([FM, "count-tokens", "-q"], input=text, capture_output=True, text=True)
    return int(out.stdout.strip())


def prose(words):
    """`words` words of real prose: the repo's README and AGENTS.md, repeated."""
    source = ((REPO / "README.md").read_text() + "\n" + (REPO / "AGENTS.md").read_text()).split()
    out = []
    while len(out) < words:
        out.extend(source)
    return " ".join(out[:words])


INCIDENTS = [
    ("payment", "ERROR payments: card gateway timed out after 30 s for order 88412; 412 orders failed (incident PAY-7731)"),
    ("disk", "ERROR storage: disk full on db-replica-2, writes rejected, replication stopped (incident DSK-2209)"),
    ("certificate", "ERROR tls: certificate for api.internal expired, 1,930 client handshakes refused (incident TLS-5512)"),
]


def log_text(lines, seed=7):
    """Timestamped service logs with three planted incidents, spread through the text."""
    rng = random.Random(seed)
    services = ["auth", "orders", "search", "billing", "cache", "gateway"]
    events = ["request served", "cache hit", "cache miss", "token refreshed", "job queued", "job done"]
    planted = {lines // 6: 0, lines // 2: 1, (lines * 5) // 6: 2}
    out = []
    for i in range(lines):
        stamp = f"2026-10-0{1 + i // 400}T{(i // 60) % 24:02d}:{i % 60:02d}:{rng.randint(0, 59):02d}Z"
        if i in planted:
            out.append(f"{stamp} {INCIDENTS[planted[i]][1]}")
        else:
            out.append(
                f"{stamp} INFO {rng.choice(services)}: {rng.choice(events)} "
                f"id={rng.randrange(16**8):08x} ms={rng.randint(1, 900)} user={rng.randint(1000, 99999)}"
            )
    return "\n".join(out) + "\n"


def render(path, lines):
    args = ["osascript", "-l", "JavaScript", str(RENDER), str(path)] + lines
    subprocess.run(args, check=True, capture_output=True)
    return path


def process_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def wait_until(predicate, limit):
    deadline = time.time() + limit
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return predicate()


class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path, timeout=300):
        super().__init__("localhost", timeout=timeout)
        self.unix_path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.unix_path)


def http(sock, method, path, body=None):
    conn = UnixHTTP(str(sock))
    payload = None if body is None else json.dumps(body)
    conn.request(method, path, body=payload, headers={"Content-Type": "application/json"})
    response = conn.getresponse()
    data = response.read().decode()
    conn.close()
    return response.status, response.getheader("Content-Type") or "", data


class FmServe:
    """A bare `fm serve`, for the facts section."""

    def __init__(self, sock):
        self.sock = Path(sock)
        self.log = open(SCRATCH / f"serve-{os.getpid()}-{time.time_ns()}.log", "w+")
        self.proc = subprocess.Popen([FM, "serve", "--socket", str(self.sock)], stdout=self.log, stderr=self.log)

    def ready(self, limit=10):
        def healthy():
            try:
                return http(self.sock, "GET", "/health")[0] == 200
            except OSError:
                return False

        return wait_until(healthy, limit)

    def output(self):
        self.log.flush()
        self.log.seek(0)
        return self.log.read()

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(5)
            except subprocess.TimeoutExpired:
                self.proc.kill()


class McpServer:
    """One fm-mcp process over stdio, through the MCP handshake."""

    def __init__(self, env=None):
        full_env = {**os.environ, "FM_MCP_LOG": "info", "FM_MCP_FM_PATH": FM, **(env or {})}
        self.proc = subprocess.Popen(
            [str(BINARY)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=full_env, text=True, bufsize=1,
        )
        self.stderr = []
        self.notifications = []
        self.next_id = 1
        threading.Thread(target=self._drain, daemon=True).start()
        self.request("initialize", {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "fm-mcp-smoke", "version": "0"},
        })
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _drain(self):
        for line in self.proc.stderr:
            self.stderr.append(line)

    def _send(self, message):
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params):
        rid = self.next_id
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("fm-mcp closed stdout:\n" + "".join(self.stderr))
            message = json.loads(line)
            if message.get("id") == rid:
                return message
            if "id" not in message:
                self.notifications.append(message)

    def call(self, tool, arguments, progress=False):
        """Returns (is_error, text, structured, seconds)."""
        params = {"name": tool, "arguments": arguments}
        if progress:
            params["_meta"] = {"progressToken": f"p{self.next_id}"}
        start = time.time()
        response = self.request("tools/call", params)
        seconds = time.time() - start
        result = response.get("result")
        if result is None:
            return True, f"protocol error: {response.get('error')}", None, seconds
        text = "".join(part.get("text", "") for part in result.get("content", []))
        return bool(result.get("isError")), text, result.get("structuredContent"), seconds

    def fm_serve_children(self):
        found = []
        for line in self.stderr:
            m = re.search(r"fm serve started pid=(\d+) socket=(\S+)", line)
            if m:
                found.append((int(m.group(1)), Path(m.group(2))))
        return found

    def close(self, limit=10):
        if self.proc.poll() is None:
            self.proc.stdin.close()
            try:
                self.proc.wait(limit)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        return self.proc.returncode


# ---------------------------------------------------------------- facts


def facts():
    print("\nFacts about fm (AGENTS.md, verified facts)")
    out = subprocess.run([FM, "available"], capture_output=True, text=True)
    check(out.returncode == 0 and "System model available" in out.stdout,
          "fm available", out.stdout.strip() or out.stderr.strip())
    out = subprocess.run([FM, "license", "--status"], capture_output=True, text=True)
    check(out.stdout.strip().startswith("Agreed"), "fm license --status", first_line(out.stdout + out.stderr))
    out = subprocess.run([FM, "count-tokens", "-q"], input="", capture_output=True, text=True)
    check(out.returncode != 0 and "Missing prompt." in out.stdout + out.stderr,
          "count-tokens on empty input fails with \"Missing prompt.\"", first_line(out.stdout + out.stderr))

    base = Path(tempfile.mkdtemp(prefix="fs-", dir="/tmp"))
    serve = FmServe(base / "fm.sock")
    try:
        if not check(serve.ready(), "fm serve answers GET /health"):
            return
        status, _, body = http(serve.sock, "GET", "/health")
        health = json.loads(body)
        check(health.get("status") == "fm serve is running" and health["models"][0]["available"] is True,
              "health body", body)
        check(serve.output() == "", "fm serve prints nothing on startup", repr(serve.output()[:200]))
        status, _, body = http(serve.sock, "GET", "/v1/models")
        check(status == 200 and '"system"' in body, "GET /v1/models lists `system`", body[:120])

        ask = {"model": "system", "messages": [{"role": "user", "content": "Say hello in three words."}]}
        status, ctype, body = http(serve.sock, "POST", "/v1/chat/completions", ask)
        check(status == 200 and body.lstrip().startswith("data:"),
              "streams by default without stream:false", f"{status} {ctype} {body[:60]!r}")
        status, _, body = http(serve.sock, "POST", "/v1/chat/completions", dict(ask, stream=False))
        reply = json.loads(body) if status == 200 else {}
        check(status == 200 and "usage" in reply and reply["choices"][0]["message"]["content"],
              "stream:false returns JSON with usage", body[:120])

        tools = [{"type": "function", "function": {"name": "get_time", "description": "Gets the time",
                                                   "parameters": {"type": "object", "properties": {}}}}]
        status, _, body = http(serve.sock, "POST", "/v1/chat/completions",
                               dict(ask, stream=False, tools=tools,
                                    messages=[{"role": "user", "content": "What time is it? Use a tool."}]))
        check(status == 200 and "tool_calls" not in body, "`tools` is ignored, not rejected", f"{status}")
        status, _, body = http(serve.sock, "POST", "/v1/chat/completions",
                               dict(ask, stream=False, logprobs=True, top_logprobs=5))
        check(status == 200 and '"logprobs"' not in body.replace('"logprobs":null', ""),
              "no log probabilities", f"{status}")

        words = 5800  # about 9.3K tokens of prose: over the 8K context
        text = prose(words)
        tokens = count_tokens(text)
        status, _, body = http(serve.sock, "POST", "/v1/chat/completions",
                               {"model": "system", "stream": False,
                                "messages": [{"role": "user", "content": "Summarise:\n" + text}]})
        check(status == 500 and "exceeded the model's context size" in body,
              f"{tokens}-token prompt gives the context overflow error", f"{status} {body[:140]}")
    finally:
        serve.stop()
    check(wait_until(lambda: not serve.sock.exists(), 3), "fm serve removes its socket on SIGTERM")
    shutil.rmtree(base, ignore_errors=True)

    long_dir = Path("/tmp") / ("fm-smoke-long-" + "x" * 90)
    long_dir.mkdir(exist_ok=True)
    sock = long_dir / "fm.sock"
    serve = FmServe(sock)
    time.sleep(3)
    check(serve.proc.poll() is None and not sock.exists(),
          f"a {len(str(sock))}-byte socket path fails silently (running, no socket file)")
    serve.stop()
    shutil.rmtree(long_dir, ignore_errors=True)


# ---------------------------------------------------------------- tools

INVOICE = """Invoice INV-20417 from Harbour Supplies Pty Ltd, issued 2 April 2026.
Bill to: Northwind Cafe, 14 Quay Street, Sydney.
2 x espresso grinder burr set at 120.00 each, 1 x milk jug 1L at 31.50.
Total due: 271.50 AUD by 2 May 2026. Pay by bank transfer."""

INVOICE_SCHEMA = {
    "type": "object",
    "properties": {
        "invoice_number": {"type": "string"},
        "customer": {"type": "string", "description": "who the invoice is billed to"},
        "total": {"type": "number"},
        "purchase_order": {"type": "string", "description": "the customer's PO number"},
    },
}

# Missing fields unrelated to anything in the text: these should always be null.
SHIPPING = ("Good news! Your order #55120 shipped on 3 October with AusPost Express. In the box: "
            "1 x brass desk lamp, 2 x LED bulbs. Expected delivery 6 to 7 October.")
SHIPPING_SCHEMA = {
    "type": "object",
    "properties": {
        "order_number": {"type": "string"},
        "carrier": {"type": "string"},
        "tracking_number": {"type": "string"},
        "delivery_address": {"type": "string"},
    },
}

TICKETS = [
    ("I was charged twice for my subscription this month, please refund one.", "billing"),
    ("The app crashes every time I open the settings page on my iPad.", "bug"),
    ("Could you add a dark mode to the dashboard?", "feature request"),
]
LABELS = ["billing", "bug", "feature request", "account access"]

ORPHAN_NOTE = "fm serve pid and socket folder gone"


def tools():
    print("\nTools through fm-mcp (real fm)")
    server = McpServer()
    try:
        listed = server.request("tools/list", {})["result"]["tools"]
        names = sorted(t["name"] for t in listed)
        check(names == ["classify", "extract", "ocr", "summarise"], "tools/list", ", ".join(names))

        notes = ("Release notes, version 4.2. Search is now twice as fast because results are cached. "
                 "Exports to CSV no longer drop the last row. The legacy XML API is removed; "
                 "use the JSON API instead. Two-factor login is now required for admins.")
        err, text, _, secs = server.call("summarise", {"text": notes, "length": "short"})
        check(not err and len(text) > 20, f"summarise ({secs:.1f} s)", text.replace("\n", " ")[:160])

        err, text, data, secs = server.call("extract", {"text": SHIPPING, "schema": SHIPPING_SCHEMA})
        data = data or {}
        exact = (data.get("order_number") in ("55120", "#55120") and "AusPost" in str(data.get("carrier"))
                 and data.get("tracking_number") is None and data.get("delivery_address") is None)
        check(not err and exact, f"extract, missing fields come back null ({secs:.1f} s)",
              json.dumps(data) if data else text[:160])
        # A missing field close in meaning to other text (PO number next to line items) can
        # take that text instead of null (measured 2026-10-08). Report the rate.
        runs, nulls, present = 5, 0, 0
        for _ in range(runs):
            err, text, data, _ = server.call("extract", {"text": INVOICE, "schema": INVOICE_SCHEMA})
            data = data or {}
            nulls += data.get("purchase_order", "") is None
            present += (data.get("invoice_number") == "INV-20417" and data.get("total") == 271.5
                        and "Northwind" in str(data.get("customer")))
        check(present == runs, f"extract, present fields right ({present}/{runs})")
        report("PASS" if nulls == runs else "WARN", f"extract, a missing field near related text is null ({nulls}/{runs})")

        valid, right, slowest = 0, 0, 0.0
        for ticket, expected in TICKETS:
            err, text, data, secs = server.call("classify", {"text": ticket, "labels": LABELS})
            label = (data or {}).get("label")
            valid += (not err and label in LABELS)
            right += label == expected
            slowest = max(slowest, secs)
        check(valid == len(TICKETS), f"classify gives valid labels ({valid}/{len(TICKETS)}, slowest {slowest:.1f} s)")
        report("PASS" if right == len(TICKETS) else "WARN", f"classify picks the expected label ({right}/{len(TICKETS)})")
        err, text, data, _ = server.call("classify", {
            "text": "I can't log in since you charged my card twice.", "labels": LABELS, "multi": True})
        labels = (data or {}).get("labels") or []
        check(not err and labels and set(labels) <= set(LABELS), "classify multi", json.dumps(labels))

        receipt = render(SCRATCH / "receipt.png", ["Harbour Cafe", "Flat white 4.80", "Blueberry muffin 5.20", "TOTAL 10.00"])
        err, text, _, secs = server.call("ocr", {"path": str(receipt)})
        phrases = ["Harbour Cafe", "Flat white", "4.80", "Blueberry muffin", "5.20", "TOTAL", "10.00"]
        found = [p for p in phrases if p.lower() in text.lower()]
        check(not err and len(found) == len(phrases), f"ocr ({len(found)}/{len(phrases)} phrases, {secs:.1f} s)",
              text.replace("\n", " | ")[:160])
        blank = render(SCRATCH / "blank.png", [])
        err, text, _, _ = server.call("ocr", {"path": str(blank)})
        report("INFO", "ocr on a blank image (F11)", ("error: " if err else "answer: ") + text.replace("\n", " ")[:160])

        err, text, _, _ = server.call("extract", {"text": prose(5000), "schema": INVOICE_SCHEMA})
        check(err and text.startswith("Input is too long"), "extract refuses 5,000 words as too long", text[:120])

        pid, sock = server.fm_serve_children()[-1]
        os.kill(pid, signal.SIGKILL)
        wait_until(lambda: not process_alive(pid), 3)
        err, text, _, secs = server.call("summarise", {"text": notes, "length": "short"})
        children = server.fm_serve_children()
        check(not err and len(children) >= 2 and children[-1][0] != pid,
              f"fm serve killed with -9, next call restarts it ({secs:.1f} s)")
    finally:
        children = server.fm_serve_children()
        code = server.close()
    pid, sock = children[-1]
    check(code == 0 and wait_until(lambda: not process_alive(pid) and not sock.parent.exists(), 5),
          "stdin closed: fm-mcp exits 0, " + ORPHAN_NOTE)

    server = McpServer()
    server.call("summarise", {"text": notes, "length": "short"})
    pid, sock = server.fm_serve_children()[-1]
    server.proc.kill()
    check(wait_until(lambda: not process_alive(pid) and not sock.parent.exists(), 5),
          "fm-mcp killed with -9: the watchdog stops fm serve, " + ORPHAN_NOTE)


# ---------------------------------------------------------------- sessions


def sessions():
    print("\nSeveral sessions at once")
    servers = [McpServer() for _ in range(3)]
    try:
        for s in servers:  # start each fm serve before timing
            s.call("classify", {"text": "warm up", "labels": ["a", "b"]})
        text = prose(2300)
        tokens = count_tokens(text)
        finished = [None] * 3
        start = time.time()

        def run(i):
            err, out, _, _ = servers[i].call("summarise", {"text": text})
            finished[i] = (time.time() - start, err, out[:100])

        threads = [threading.Thread(target=run, args=(i,)) for i in range(3)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        times = sorted(f[0] for f in finished)
        failed = [f for f in finished if f[1]]
        check(not failed, f"3 sessions, one {tokens}-token summarise each, all succeed",
              "finished at " + ", ".join(f"{t:.1f} s" for t in times) + "".join(f"; error: {f[2]}" for f in failed))
    finally:
        for s in servers:
            s.close()

    print("\nLong summarise in one session, extract from another (F2, summary coverage)")
    log = log_text(700)
    tokens = count_tokens(log)
    long_session, other = McpServer(), McpServer()
    try:
        other.call("classify", {"text": "warm up", "labels": ["a", "b"]})
        outcome = {}

        def long_run():
            outcome["result"] = long_session.call("summarise", {"text": log, "focus": "errors and incidents"}, progress=True)

        thread = threading.Thread(target=long_run)
        thread.start()
        time.sleep(5)
        extracts = []
        while thread.is_alive():
            err, text, _, secs = other.call("extract", {"text": INVOICE, "schema": INVOICE_SCHEMA})
            extracts.append((err, text, secs))
            time.sleep(3)
        thread.join()
        err, summary, _, secs = outcome["result"]
        check(not err, f"summarise of a {tokens}-token log ({secs:.0f} s, "
              f"{len(long_session.notifications)} progress updates)", summary[:120] if err else "")
        stuck = [e for e in extracts if e[0] and "got stuck" in e[1]]
        other_errors = [e for e in extracts if e[0] and "got stuck" not in e[1]]
        slowest = max((e[2] for e in extracts), default=0)
        report("PASS" if not stuck else "WARN",
               f"{len(extracts)} extract calls meanwhile: {len(stuck)} \"got stuck\", slowest {slowest:.1f} s")
        check(not other_errors, "no other extract errors meanwhile", "; ".join(e[1][:80] for e in other_errors))
        if not err:
            kept = [key for key, _ in INCIDENTS if key in summary.lower()]
            report("PASS" if len(kept) == len(INCIDENTS) else "WARN",
                   f"summary kept {len(kept)} of {len(INCIDENTS)} planted incidents", ", ".join(kept))
            (SCRATCH / "long-summary.txt").write_text(summary)
            report("INFO", "full summary saved", str(SCRATCH / "long-summary.txt"))
    finally:
        long_session.close()
        other.close()


# ---------------------------------------------------------------- main


def main():
    sections = sys.argv[1:] or ["facts", "tools"]
    known = {"facts": facts, "tools": tools, "sessions": sessions}
    unknown = [s for s in sections if s not in known]
    if unknown:
        sys.exit(f"unknown section {unknown[0]}; choose from: {', '.join(known)}")
    if not BINARY.exists():
        sys.exit(f"{BINARY} not found; run `cargo build --release` first")
    print(f"fm-mcp smoke test: {BINARY}, fm {FM}, scratch {SCRATCH}")
    for name in sections:
        try:
            known[name]()
        except Exception as e:  # report and carry on with the next section
            report("FAIL", f"{name} section stopped", repr(e))
    failed = [r for r in results if r[0] == "FAIL"]
    warned = [r for r in results if r[0] == "WARN"]
    passed = sum(r[0] == "PASS" for r in results)
    print(f"\n{passed} passed, {len(failed)} failed, {len(warned)} warnings")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
