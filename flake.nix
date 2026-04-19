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
    let
      base = (import "${substrate}/lib/rust-workspace-release-flake.nix" {
        inherit nixpkgs crate2nix flake-utils;
      }) {
        toolName = "frost";
        packageName = "frost";
        src = self;
        repo = "pleme-io/frost";
      };

      # Expose `frost-complete-forge` as a secondary package. crate2nix
      # makes every workspace member's binaries available via
      # `project.workspaceMembers.<name>.build`. We just wire it into
      # the per-system packages set so frostmourne (and anyone else who
      # depends on frost) can bundle the forge alongside frost itself.
      forgeOverlay = flake-utils.lib.eachDefaultSystem (system: let
        pkgs = import nixpkgs { inherit system; };
        crate2nixPkg = crate2nix.packages.${system}.default;
        project = pkgs.callPackage ./Cargo.nix {
          defaultCrateOverrides = pkgs.defaultCrateOverrides;
        };
        forgePkg = project.workspaceMembers."frost-complete".build;
      in {
        packages.frost-complete-forge = forgePkg;
        # Expose crate2nix so consumers who want to generate specs at
        # runtime have the binary in their closure (frostmourne uses it).
        packages.crate2nix = crate2nixPkg;
      });
    in
      nixpkgs.lib.recursiveUpdate base forgeOverlay;
}
