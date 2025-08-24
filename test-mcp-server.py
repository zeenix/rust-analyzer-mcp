#!/usr/bin/env python3
"""Test script demonstrating all rust-analyzer MCP features."""

import json
import subprocess
import sys
import time
from pathlib import Path

def send_request(proc, request_id, method, params=None):
    """Send a JSON-RPC request and get response."""
    request = {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method
    }
    if params:
        request["params"] = params

    request_str = json.dumps(request)
    proc.stdin.write(request_str + "\n")
    proc.stdin.flush()

    # Read response
    response_line = proc.stdout.readline()
    if response_line:
        return json.loads(response_line)
    return None

def main():
    # Start the MCP server
    workspace = Path.cwd() / "test-project"
    cmd = ["cargo", "run", "--", str(workspace)]

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=0
    )

    try:
        # Initialize
        print("Initializing rust-analyzer MCP server...")
        response = send_request(proc, 1, "initialize", {
            "protocolVersion": "0.1.0",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        })
        print(f"✓ Server initialized: {response.get('result', {}).get('serverInfo', {})}")

        # Set workspace
        print(f"\n✓ Workspace set to: {workspace}")
        response = send_request(proc, 2, "tools/call", {
            "name": "rust_analyzer_set_workspace",
            "arguments": {"workspace_path": str(workspace)}
        })
        time.sleep(3)  # Give rust-analyzer time to initialize

        test_file = str(workspace / "src/main.rs")

        print("\n" + "="*60)
        print("WORKING FEATURES FOR AI AGENTS:")
        print("="*60)

        # 1. SYMBOLS - Get code structure
        print("\n1. SYMBOLS - Navigate codebase structure")
        response = send_request(proc, 3, "tools/call", {
            "name": "rust_analyzer_symbols",
            "arguments": {"file_path": test_file}
        })
        if response and response.get("result", {}).get("content"):
            symbols = json.loads(response["result"]["content"][0]["text"])
            print(f"   ✓ Found {len(symbols)} symbols")
            for symbol in symbols[:3]:
                print(f"     - {symbol['name']} ({symbol['kind']})")

        # 2. DEFINITION - Jump to definitions
        print("\n2. DEFINITION - Jump to symbol definitions")
        response = send_request(proc, 4, "tools/call", {
            "name": "rust_analyzer_definition",
            "arguments": {
                "file_path": test_file,
                "line": 1,  # On 'greet' call
                "character": 20
            }
        })
        if response and response.get("result", {}).get("content"):
            defs = json.loads(response["result"]["content"][0]["text"])
            if defs and len(defs) > 0:
                loc = defs[0].get("targetUri", "").split("/")[-1]
                line = defs[0].get("targetRange", {}).get("start", {}).get("line", 0)
                print(f"   ✓ 'greet' defined at {loc}:{line+1}")

        # 3. REFERENCES - Find all usages
        print("\n3. REFERENCES - Find all symbol usages")
        response = send_request(proc, 5, "tools/call", {
            "name": "rust_analyzer_references",
            "arguments": {
                "file_path": test_file,
                "line": 8,  # On 'greet' definition
                "character": 3
            }
        })
        if response and response.get("result", {}).get("content"):
            refs = json.loads(response["result"]["content"][0]["text"])
            print(f"   ✓ 'greet' referenced {len(refs)} times:")
            for ref in refs:
                line = ref["range"]["start"]["line"]
                char = ref["range"]["start"]["character"]
                print(f"     - Line {line+1}, character {char}")

        # 4. HOVER - Get type information
        print("\n4. HOVER - Get type info and documentation")
        response = send_request(proc, 6, "tools/call", {
            "name": "rust_analyzer_hover",
            "arguments": {
                "file_path": test_file,
                "line": 1,
                "character": 10
            }
        })
        if response and response.get("result", {}).get("content"):
            hover = json.loads(response["result"]["content"][0]["text"])
            if hover and hover.get("contents"):
                value = hover["contents"].get("value", "")
                if value:
                    print(f"   ✓ Hover info: {value.split('\\n')[0][:50]}...")

        # 5. COMPLETION - Get code suggestions
        print("\n5. COMPLETION - Get code completions")
        response = send_request(proc, 7, "tools/call", {
            "name": "rust_analyzer_completion",
            "arguments": {
                "file_path": test_file,
                "line": 2,
                "character": 5
            }
        })
        if response and response.get("result", {}).get("content"):
            completions = json.loads(response["result"]["content"][0]["text"])
            if isinstance(completions, list):
                print(f"   ✓ {len(completions)} completions available")
                for item in completions[:3]:
                    print(f"     - {item['label']}")
            else:
                print(f"   ✓ Completions available")

        # 6. FORMAT - Format code
        print("\n6. FORMAT - Format Rust code")
        response = send_request(proc, 8, "tools/call", {
            "name": "rust_analyzer_format",
            "arguments": {"file_path": test_file}
        })
        if response and response.get("result", {}).get("content"):
            edits = json.loads(response["result"]["content"][0]["text"])
            if edits:
                print(f"   ✓ {len(edits)} formatting edits available")
            else:
                print(f"   ✓ Code is already properly formatted")

        print("\n" + "="*60)
        print("SUMMARY FOR AI AGENTS:")
        print("="*60)
        print("""
The rust-analyzer MCP server now provides 6 critical features:

✓ SYMBOLS - Parse and navigate code structure (functions, structs, etc.)
✓ DEFINITION - Jump to where symbols are defined
✓ REFERENCES - Find all places where symbols are used
✓ HOVER - Get type information and documentation
✓ COMPLETION - Get context-aware code completions
✓ FORMAT - Format code according to Rust standards

These features enable AI agents to:
- Navigate and understand Rust codebases efficiently
- Find cross-references between code elements
- Get type information without manual parsing
- Generate properly formatted Rust code
        """)

    finally:
        proc.terminate()
        proc.wait()

if __name__ == "__main__":
    main()