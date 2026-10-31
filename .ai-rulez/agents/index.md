# Concepts

- [Backend Author](backend-author.md) - Implements a new poly engine backend end-to-end — empirically checks the upstream crate API, wraps it as a crates.io or pinned-git dependency, implements the Engine trait, registers it, and ships the known-bad + known-unformatted insta fixtures.
- [Rust Perf Engineer](rust-perf-engineer.md) - Reviews diffs touching poly's per-file runner / discovery / cache / engine paths for hot-path regressions — needless allocations/clones, missed borrows, parser-pool misuse, and non-rayon parallelism.
