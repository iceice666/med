"""A language server that only tells what it was told, for tests/lsp_config.rs.

It is started as `python3 fake_lsp.py NAME`. Each open file gets one
diagnostic from it, whose message is JSON: its name, the init options it was
started with, what it was answered when it asked for its configuration, what
it was sent with didChangeConfiguration, and the commands it was asked to
run. Its one code action is called after it, and runs a command of its own.
"""

import json
import sys

NAME = sys.argv[1]
seen = {"name": NAME, "init": None, "asked": None, "pushed": None, "ran": []}
files = []


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        name, _, value = line.partition(b":")
        if name.lower() == b"content-length":
            length = int(value)
    return json.loads(sys.stdin.buffer.read(length))


def publish():
    for uri in files:
        send({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": uri, "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0},
                          "end": {"line": 0, "character": 1}},
                "severity": 2,
                "message": json.dumps(seen, sort_keys=True),
            }]},
        })


while True:
    message = read()
    if message is None:
        break
    method = message.get("method")
    params = message.get("params") or {}
    if method == "initialize":
        seen["init"] = params.get("initializationOptions")
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {
            "textDocumentSync": 1,
            "codeActionProvider": True,
            "executeCommandProvider": {"commands": ["fake.run"]},
        }}})
    elif method == "initialized":
        send({"jsonrpc": "2.0", "id": "config", "method": "workspace/configuration",
              "params": {"items": [{"section": "fake.sub"}, {"scopeUri": "file:///"}]}})
    elif method is None and message.get("id") == "config":
        seen["asked"] = message.get("result")
        publish()
    elif method == "workspace/didChangeConfiguration":
        seen["pushed"] = params.get("settings")
        publish()
    elif method == "textDocument/didOpen":
        files.append(params["textDocument"]["uri"])
        publish()
    elif method == "textDocument/codeAction":
        send({"jsonrpc": "2.0", "id": message["id"], "result": [{
            "title": "from " + NAME,
            "command": {"title": "from " + NAME, "command": "fake.run",
                        "arguments": [NAME]},
        }]})
    elif method == "workspace/executeCommand":
        seen["ran"].append(params.get("arguments"))
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
        publish()
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
    elif method == "exit":
        break
    elif "id" in message and method is not None:
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
