# poly over the Kubernetes repository. The timing is real — vhs runs the command live.
Output docs/media/scale.gif

Set Shell "bash"
Set FontSize 15
Set Width 1300
Set Height 640
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms
Set PlaybackSpeed 1.0

Hide
Type "export PATH=/Users/naamanhirschfeld/workspace/poly/target/release:$PATH POLY_NO_SHADOW_WARN=1" Enter
Type "cd /tmp/poly-demo/kubernetes && clear" Enter
Sleep 1s
Show

Type "# Kubernetes: 31,303 files. Go, YAML, JSON, Markdown, shell, protobuf." Enter
Sleep 1200ms
Type "# poly is one binary. No Go toolchain, no Node, no Python." Enter
Sleep 1200ms
Type "time poly lint --no-workspace --quiet ." Enter
Sleep 30s
