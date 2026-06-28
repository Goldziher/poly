# Attributions

poly bundles, derives from, or vendors data from the third-party projects listed
below. Each is used under its original license; the relevant license text and
copyright are reproduced or linked here. Tools wrapped only as ordinary Cargo
dependencies (e.g. oxc, ruff, taplo, rumdl) are governed by their own crate
licenses and are not re-listed here — this file covers **vendored or derived**
source and data.

---

## prek — git-hook runner (derived & vendored)

- Project: <https://github.com/j178/prek>
- License: MIT
- Copyright © the prek authors

poly's native hook runner (`crates/poly-hooks`) ports execution primitives,
git helpers, file identification/tagging, the PTY handling, and the git-hook
shim + `hook-impl` stdin parsing from prek, converted to a synchronous,
rayon-driven form. `crates/polyhooks` is a vendored copy of prek retained
during the migration and removed once the native runner is complete.

```text
MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## mdsf — tool catalog data (vendored)

- Project: <https://github.com/hougesen/mdsf>
- License: MIT
- Copyright © 2024 Mads Hougesen

poly vendors tool-definition data derived from mdsf's `tools/*/plugin.json`
catalog (the mapping of tool → binary → argument vector → stdin convention →
languages/categories, and its golden input/output fixtures) to populate poly's
built-in tool registry. poly does not depend on the `mdsf` crate or invoke the
`mdsf` binary; only the catalog data is reused, under the MIT terms below.

```text
MIT License

Copyright (c) 2024 Mads Hougesen

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
