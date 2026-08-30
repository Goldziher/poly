"""Print the tool list from an MCP `tools/list` response arriving on stdin."""

# poly: allow-file[T201] printing to stdout is what this script is for

import json
import sys

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if message.get("id") != 2:
        continue
    tools = message["result"]["tools"]
    print(f"{len(tools)} tools:")
    for tool in tools:
        name = tool["name"]
        summary = tool.get("description", "")[:56]
        print(f"  {name:<20} {summary}")
