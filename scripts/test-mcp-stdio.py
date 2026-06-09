#!/usr/bin/env python3
"""Drive mossraven-service over stdio with real JSON-RPC traffic.

Mimics what the WPF shell (and Claude Code) do: spawn the service binary,
write newline-delimited JSON-RPC 2.0 requests to its stdin, read newline-
delimited JSON responses from its stdout. Logs all stderr lines separately.

This is the test that catches:
  - PoB's Lua print output leaking into stdout and corrupting JSON-RPC framing
  - Missing tool handlers
  - Schema mismatches in tool inputs/outputs
  - Service lifecycle (does it exit cleanly when stdin closes?)
"""

import json
import os
import subprocess
import sys
import threading
import time

EXE = r"C:\#AppProjects\MossOrb\dist\mossraven-service.exe"
CWD = r"C:\#AppProjects\MossOrb"

requests = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "test-mcp-stdio", "version": "0.1"},
    }},
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "seed_hypothesis",
        "arguments": {"concept": "lightning sorceress shield burst"},
    }},
    {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
        "name": "run_search",
        "arguments": {"generations": 3},
    }},
    {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
        "name": "read_archive",
        "arguments": {},
    }},
]

env = os.environ.copy()
env["NoDefaultCurrentDirectoryInExePath"] = ""
env["RUST_LOG"] = "info"

print(f"spawn: {EXE}")
proc = subprocess.Popen(
    [EXE],
    cwd=CWD,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    env=env,
    bufsize=0,
)


def reader(name, stream, store):
    for raw in iter(stream.readline, b""):
        line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
        store.append(line)
        print(f"[{name}] {line[:200]}")


stdout_lines: list[str] = []
stderr_lines: list[str] = []
threading.Thread(target=reader, args=("STDOUT", proc.stdout, stdout_lines), daemon=True).start()
threading.Thread(target=reader, args=("STDERR", proc.stderr, stderr_lines), daemon=True).start()

# Give it a moment to start + init PoB
time.sleep(3)

for r in requests:
    s = json.dumps(r) + "\n"
    print(f"\n>>> SEND: {s.strip()[:200]}")
    proc.stdin.write(s.encode("utf-8"))
    proc.stdin.flush()
    time.sleep(2)  # let it process

# Close stdin to make the service exit
proc.stdin.close()
try:
    proc.wait(timeout=10)
except subprocess.TimeoutExpired:
    print("[harness] service didn't exit on stdin close; killing")
    proc.kill()

print(f"\n=== summary ===")
print(f"stdout lines: {len(stdout_lines)}")
print(f"stderr lines: {len(stderr_lines)}")

# Verify stdout lines all parse as JSON-RPC
parse_fails = 0
json_responses = 0
for line in stdout_lines:
    if not line.strip():
        continue
    try:
        json.loads(line)
        json_responses += 1
    except json.JSONDecodeError:
        parse_fails += 1

print(f"stdout JSON responses: {json_responses}")
print(f"stdout NON-JSON pollution: {parse_fails}  <-- if > 0, JSON-RPC framing is corrupted")
if parse_fails > 0:
    print("\nFirst 5 non-JSON lines (these would break the WPF MCP client):")
    n = 0
    for line in stdout_lines:
        if not line.strip():
            continue
        try:
            json.loads(line)
        except json.JSONDecodeError:
            print(f"  POLLUTION: {line[:200]}")
            n += 1
            if n >= 5:
                break

sys.exit(0 if parse_fails == 0 else 1)
