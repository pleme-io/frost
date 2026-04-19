{
  description = "Frost — a zsh-compatible shell written in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    crate2nix.url = "github:nix-community/crate2nix";
    flake-utils.url = "github:numtide/flake-utils";
    substrate = {
      url = "github:pleme-io/substrate";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  # substrate's rust-workspace-release builder wraps crate2nix + eachSystem +
  # overlays so we don't hand-maintain cargoLock hashes, importCargoLock
  # outputHashes, or per-target `buildRustPackage` invocations. git deps
  # (eg. tatara-lisp) are vendored automatically via Cargo.nix generation.
  #
  # See `substrate/lib/build/rust/workspace-release-flake.nix` for the
  # contract. Other consumers of the same pattern: pleme-io/mamorigami.
  outputs = { self, nixpkgs, crate2nix, flake-utils, substrate, ... }:
    (import "${substrate}/lib/rust-workspace-release-flake.nix" {
      inherit nixpkgs crate2nix flake-utils;
    }) {
      toolName = "frost";
      packageName = "frost";
      src = self;
      repo = "pleme-io/frost";
    };
}
