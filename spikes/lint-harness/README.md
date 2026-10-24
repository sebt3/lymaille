# Lint harness test bench

Strict lint configuration for lymaille, ready to be moved to the repository root when the crate skeleton is created.
Also serves as the `readme` of this manifest.

- `Cargo.toml`: `[workspace.lints]` for rustc, rustdoc and clippy (groups `all`, `pedantic`, `nursery`, `cargo`, plus a selection of
  `restriction` lints chosen one by one).
- `clippy.toml`: thresholds, test allowances, and `disallowed-*` lists that encode the architecture constraints.
- `rustfmt.toml`: stable options only.
- `scripts/check`: the single entry point (`fast` or full). It needs `--all-features` to be free of canaries, so it is
  validated on a canary-free copy.
- `check.sh`: self-test. The clean base must raise no diagnostic and every expected lint must fire on the deliberate faults in
  `src/canaries.rs` (`--features canaries`), so a lint renamed or misconfigured by a toolchain update cannot go silent.
