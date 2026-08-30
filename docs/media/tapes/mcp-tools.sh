#!/usr/bin/env bash
# Speak MCP to `poly mcp` over stdio and print the tool list it advertises.
# Used by agent.tape; also a standalone check that the server handshakes.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"demo","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | poly mcp 2>/dev/null | python3 "$here/mcp-tools.py"
