# One binary across five languages in one repository, lint and format.
Output docs/media/polyglot.gif

Set Shell "bash"
Set FontSize 15
Set Width 1300
Set Height 700
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

Hide
Type "export PATH=/Users/naamanhirschfeld/workspace/poly/target/release:$PATH POLY_NO_SHADOW_WARN=1" Enter
Type "cd /tmp/poly-demo/django && clear" Enter
Sleep 1s
Show

Type "# Django is not a Python repo. It is Python, JavaScript, CSS, HTML, TOML, YAML, Markdown." Enter
Sleep 1800ms
Type "time poly fmt --check --quiet ." Enter
Sleep 6s
Type "clear" Enter
Sleep 500ms
Type "# One tool, one config, one pass. No per-language runner to install." Enter
Sleep 1800ms
Type "time poly lint --no-workspace --quiet ." Enter
Sleep 10s
