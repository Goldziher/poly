#!/usr/bin/env python3
"""Generate the README's catalog collapsible directly from the vendored catalog.

Reads ``crates/poly-catalog/data/catalog.json`` (the embedded mdsf registry) and
rewrites the block between the ``<!-- BEGIN CATALOG -->`` / ``<!-- END CATALOG -->``
markers in ``README.md`` with a ``<details>`` table of every catalog tool.

Run after the catalog is revendored::

    python3 scripts/gen-catalog-md.py
"""

from __future__ import annotations

import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CATALOG = ROOT / "crates" / "poly-catalog" / "data" / "catalog.json"
README = ROOT / "README.md"

BEGIN = "<!-- BEGIN CATALOG -->"
END = "<!-- END CATALOG -->"


def render() -> str:
    tools = json.loads(CATALOG.read_text())
    tools.sort(key=lambda t: t["name"].lower())

    languages = sorted({lang for tool in tools for lang in tool.get("languages", [])})

    lines: list[str] = [
        BEGIN,
        "",
        "<details>",
        "<summary><strong>Embedded tool catalog "
        f"({len(tools)} tools across {len(languages)} languages)</strong></summary>",
        "",
        "<!-- markdownlint-disable MD013 -->",
        "",
        "Opt in per tool with `[tools.<name>] enabled = true`. Each command is "
        "probed on `PATH` and skipped when absent, so listing one never makes a run fail.",
        "",
        "| Tool | Type | Languages |",
        "|---|---|---|",
    ]

    for tool in tools:
        name = tool["name"]
        homepage = tool.get("homepage")
        label = f"[{name}]({homepage})" if homepage else name
        kind = ", ".join(tool.get("categories", [])) or "tool"
        langs = ", ".join(tool.get("languages", []))
        lines.append(f"| {label} | {kind} | {langs} |")

    lines += [
        "",
        "<!-- markdownlint-enable MD013 -->",
        "",
        "</details>",
        "",
        END,
    ]
    return "\n".join(lines)


def main() -> int:
    text = README.read_text()
    if BEGIN not in text or END not in text:
        sys.stderr.write(f"markers {BEGIN!r} / {END!r} not found in {README}; add them first\n")
        return 1

    head, _, rest = text.partition(BEGIN)
    _, _, tail = rest.partition(END)
    README.write_text(head + render() + tail)
    print(f"updated catalog block in {README.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
