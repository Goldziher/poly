# Known-bad fixture for the `quality` engine's `lazy-ignore` rule.
# Every marker it still scans for is another tool's suppression syntax, so
# the rule cannot be exercised from the Rust fixture next to this one.
import os  # noqa

# eslint and biome markers are scanned in any language: the rule is a text
# scan, not a parse, and a repo's JS config can be commented in a .py file.
value = 1  # noqa: F401 re-exported on purpose, so this line must stay clean
