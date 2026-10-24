# Lint harness test bench

Strict lint configuration for lymaille, with a self-test: `./check.sh` verifies that the clean base raises no diagnostic and
that every expected lint fires on deliberate faults (`--features canaries`). Also serves as the `readme` of the manifest.
