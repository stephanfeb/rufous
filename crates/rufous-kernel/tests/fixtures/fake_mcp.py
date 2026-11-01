# Minimal stdio MCP server for rufous's tests: line-delimited JSON-RPC, echoes request ids.
# Tools: echo(text) -> "echo: <text>", needs_q(q) -> errors with -32602 unless q is given.
import json, sys

TOOLS = [
    {"name": "echo", "description": "Echo text back\nsecond line", "inputSchema": {
        "type": "object", "properties": {"text": {"type": "string"}, "times": {"type": "integer"}},
        "required": ["text"]}},
    {"name": "needs_q", "description": "d" * 80, "inputSchema": {
        "type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]}},
    {"name": "write_file", "description": "dup of a builtin", "inputSchema": {"type": "object", "properties": {}}},
    {"name": "save_path", "description": "returns the path it was given", "inputSchema": {
        "type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}},
]

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for ln in sys.stdin:
    try:
        msg = json.loads(ln)
    except Exception:
        continue
    mid, method = msg.get("id"), msg.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": msg.get("params", {}).get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS}})
    elif method == "tools/call":
        p = msg.get("params", {}); name = p.get("name"); args = p.get("arguments") or {}
        if name == "echo":
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "echo: " + str(args.get("text"))}]}})
        elif name == "needs_q":
            if "q" in args:
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "ok " + args["q"]}]}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": "Invalid params: q is required"}})
        elif name == "save_path":
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "path=" + args.get("path", "")}]}})
        else:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "unknown tool"}})
    elif method == "ping":
        send({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}})
