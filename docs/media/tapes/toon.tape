# The same run, in a shape an agent reads: --format toon.
Output docs/media/toon.gif

Set Shell "bash"
Set FontSize 15
Set Width 1300
Set Height 700
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

Hide
Type "export PATH=/Users/naamanhirschfeld/workspace/poly/target/release:$PATH POLY_NO_SHADOW_WARN=1" Enter
Type "cd /tmp/poly-demo/django/django/utils && clear" Enter
Sleep 1s
Show

Type "# Every report has a machine-readable form: --format json, or TOON." Enter
Sleep 1200ms
Type "# TOON is compact enough to hand an agent without burning its context." Enter
Sleep 1500ms
Type "poly lint --no-workspace --format toon deconstruct.py | head -30" Enter
Sleep 6s
