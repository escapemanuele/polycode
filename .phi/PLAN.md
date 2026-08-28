# Plan

**Goal:** Unblock CI quality gate on PR #11 (M13d3 TUI visual polish) by clearing pre-existing clippy lints surfaced by CI's newer stable toolchain, then merge the green PR.

- [o] Apply 3 mechanical clippy fixes in src/providers/claude: narrow `use std::io::{Read as _, Seek as _, SeekFrom}` to just `SeekFrom` (unused even under all features), and convert two `.ok().is_some_and(F)` calls at protocol.rs:116 & :122 to `.is_ok_and(F)`. These are pre-existing, surfaced by CI's newer stable + `-D warnings --all-features`, NOT introduced by M13d3.
- [ ] Format + verify locally: run `cargo fmt` then `cargo fmt --check` (expect clean) and `cargo test --all-features --no-run` to confirm the removed imports don't break compilation under all features — local clippy 1.97 can't see these newer lints, so an all-features compile is the local safety net.
- [ ] Commit as a separate `fix(clippy)` commit (keeping M13d3's TUI work conceptually clean), push to update PR #11, and watch the CI quality run go green.
- [ ] If green: merge PR #11 with a merge commit and confirm merged state. If CI surfaces MORE newer-toolchain lints not reproducible locally, pause to consider installing rustup stable for local parity before iterating.
