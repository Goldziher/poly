# poly as an MCP server: an agent calls its tools instead of parsing terminal text.
Output docs/media/agent.gif

Set Shell "bash"
Set FontSize 15
Set Width 1300
Set Height 560
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

Hide
Type "export PATH=/Users/naamanhirschfeld/workspace/poly/target/release:$PATH POLY_NO_SHADOW_WARN=1" Enter
Type "cd /tmp/poly-demo/django && clear" Enter
Sleep 1s
Show

Type "# poly ships a Claude/Codex plugin and a stdio MCP server: poly mcp" Enter
Sleep 1800ms
Type "# An agent speaks MCP to it and gets structured results, not terminal text." Enter
Sleep 1800ms
Type "bash ~/workspace/poly/docs/media/tapes/mcp-tools.sh" Enter
Sleep 8s
