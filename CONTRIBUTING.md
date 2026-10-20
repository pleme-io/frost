# Contributing to frost

1. Fork and branch from `main`.
2. `cargo fmt --all && cargo test --workspace --lib` must pass; zsh behaviour
   changes also run `cargo test --test ztst_bridge`.
3. Open a pull request. Every merge to `main` is released automatically: the
   workspace version is bumped, tagged, published to crates.io, and prebuilt
   binaries are attached to the GitHub release.

Behaviour that diverges from zsh needs a test that states the divergence.
