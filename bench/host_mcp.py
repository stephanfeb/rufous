#!/usr/bin/env python3
"""A stand-in for Choreo's MCP server (bead boubou-c9a): streamable HTTP on 127.0.0.1, one
tool per "automation", every call recorded. `say_hello` is always first; the other tools are
plausible automations with descriptions the length Choreo writes, so a sweep over the tool
count measures what host tools cost a small model (docs/host-tools.md, "the cap").

    python3 bench/host_mcp.py --tools 10 --port 8765     # serve until killed

The bench imports `HostServer` and runs it in a thread.
"""
import argparse
import json
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = "bench-token"

# (name, description, {param: description}); say_hello first, then in the order a sweep adds them.
AUTOMATIONS = [
    ("say_hello", "Say Hello: greets someone by name and returns the greeting. Needs who, the name of the person to greet.",
     {"who": "name of the person to greet"}),
    ("weekly_report", "Weekly Report: collects this week's finished tasks from the task list and writes a one-page summary to Documents/Reports. Needs nothing.", {}),
    ("tidy_downloads", "Tidy Downloads: moves files older than 30 days from Downloads into dated folders under Archive. Needs nothing; reports how many files moved.", {}),
    ("summarise_inbox", "Summarise Inbox: reads unread mail from the last day and returns a short summary per sender. Needs nothing; no mail is sent or changed.", {}),
    ("backup_photos", "Backup Photos: copies new photos from Pictures to the external drive named Backup. Needs nothing; fails if the drive is not connected.", {}),
    ("post_standup", "Post Standup: posts yesterday's done items and today's plan to the team chat channel. Needs text, the standup message to post.",
     {"text": "the standup message"}),
    ("check_website", "Check Website: fetches a web page and reports its status code and load time. Needs url, the address to check.",
     {"url": "the address to check"}),
    ("resize_images", "Resize Images: resizes every image in a folder to a maximum width. Needs folder and width in pixels.",
     {"folder": "folder of images", "width": "maximum width in pixels"}),
    ("invoice_reminder", "Invoice Reminder: finds unpaid invoices older than 14 days and drafts a polite reminder email for each. Drafts only; nothing is sent.", {}),
    ("meeting_notes", "Meeting Notes: turns the latest voice memo into written meeting notes with action items. Needs nothing.", {}),
    ("expense_log", "Expense Log: adds an expense row to the Expenses spreadsheet. Needs amount, currency and what it was for.",
     {"amount": "amount spent", "currency": "currency code", "what": "what it was for"}),
    ("focus_mode", "Focus Mode: turns on Do Not Disturb, closes chat apps and starts a timer. Needs minutes, how long to focus.",
     {"minutes": "how long to focus"}),
    ("rss_digest", "RSS Digest: gathers today's items from the saved news feeds and returns a digest of headlines. Needs nothing.", {}),
    ("clean_desktop", "Clean Desktop: moves everything on the Desktop into a dated folder under Documents/Desktop Archive. Needs nothing.", {}),
    ("water_plants", "Water Plants: sends the reminder to water the plants to the user's phone. Needs nothing.", {}),
    ("translate_text", "Translate Text: translates a piece of text into another language on this Mac. Needs text and language.",
     {"text": "text to translate", "language": "target language"}),
    ("backup_notes", "Backup Notes: exports all notes to Markdown files in the Backups folder. Needs nothing.", {}),
    ("track_package", "Track Package: looks up a parcel's delivery status. Needs tracking, the tracking number.",
     {"tracking": "tracking number"}),
    ("daily_agenda", "Daily Agenda: lists today's calendar events and due reminders in time order. Needs nothing.", {}),
    ("rename_scans", "Rename Scans: renames scanned PDFs in the Scans folder by their date and first heading. Needs nothing.", {}),
    ("pomodoro_log", "Pomodoro Log: records a finished focus session in the time log. Needs task, what was worked on.",
     {"task": "what was worked on"}),
    ("git_pull_all", "Git Pull All: pulls the latest changes in every repository under Code. Needs nothing; reports failures.", {}),
    ("screenshot_sort", "Screenshot Sort: moves screenshots from the Desktop into a Screenshots folder by month. Needs nothing.", {}),
    ("birthday_check", "Birthday Check: lists contacts with a birthday in the next seven days. Needs nothing.", {}),
    ("disk_report", "Disk Report: reports free disk space and the ten largest folders in the home folder. Needs nothing.", {}),
]


def tool_list(n):
    out = []
    for name, desc, params in AUTOMATIONS[:n]:
        out.append({"name": name, "description": desc, "inputSchema": {
            "type": "object",
            "properties": {k: {"type": "string", "description": v} for k, v in params.items()},
            "required": list(params),
        }})
    return out


def result_text(name, args, run_id):
    if name == "say_hello":
        return f"Hello, {args.get('who') or 'there'}! (run {run_id})"
    return f"{name} finished (run {run_id})."


class HostServer:
    def __init__(self, n_tools, port=0):
        self.tools = tool_list(n_tools)
        self.calls = []
        self.lock = threading.Lock()
        srv = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def _json(self, code, body, extra=None):
                data = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                for k, v in (extra or {}).items():
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                self.send_response(405)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def do_DELETE(self):
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def do_POST(self):
                if self.headers.get("Authorization") != f"Bearer {TOKEN}":
                    return self._json(401, {"error": "unauthorized"})
                raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
                msg = json.loads(raw or b"{}")
                if "id" not in msg:
                    self.send_response(202)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                method, mid = msg.get("method"), msg["id"]
                p = msg.get("params") or {}
                if method == "initialize":
                    return self._json(200, {"jsonrpc": "2.0", "id": mid, "result": {
                        "protocolVersion": p.get("protocolVersion", "2025-03-26"),
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "choreo-bench", "version": "0"}}},
                        {"Mcp-Session-Id": uuid.uuid4().hex})
                if method == "tools/list":
                    return self._json(200, {"jsonrpc": "2.0", "id": mid, "result": {"tools": srv.tools}})
                if method == "tools/call":
                    name, args = p.get("name"), p.get("arguments") or {}
                    run_id = uuid.uuid4().hex[:6]
                    with srv.lock:
                        srv.calls.append({"tool": name, "args": args, "run": run_id})
                    if not any(t["name"] == name for t in srv.tools):
                        return self._json(200, {"jsonrpc": "2.0", "id": mid, "result": {
                            "content": [{"type": "text", "text": f"no automation named {name}"}], "isError": True}})
                    return self._json(200, {"jsonrpc": "2.0", "id": mid, "result": {
                        "content": [{"type": "text", "text": result_text(name, args, run_id)}]}})
                if method == "ping":
                    return self._json(200, {"jsonrpc": "2.0", "id": mid, "result": {}})
                return self._json(200, {"jsonrpc": "2.0", "id": mid,
                                        "error": {"code": -32601, "message": f"no method {method}"}})

        self.httpd = ThreadingHTTPServer(("127.0.0.1", port), H)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)

    def start(self):
        self.thread.start()
        return self

    def stop(self):
        self.httpd.shutdown()

    def take_calls(self):
        with self.lock:
            out, self.calls = self.calls, []
        return out

    def acp_entry(self, name="choreo"):
        return {"type": "http", "name": name, "url": f"http://127.0.0.1:{self.port}/mcp",
                "headers": [{"name": "Authorization", "value": f"Bearer {TOKEN}"}]}


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tools", type=int, default=10)
    ap.add_argument("--port", type=int, default=8765)
    a = ap.parse_args()
    s = HostServer(a.tools, a.port)
    print(f"serving {a.tools} tools on http://127.0.0.1:{s.port}/mcp")
    s.httpd.serve_forever()
