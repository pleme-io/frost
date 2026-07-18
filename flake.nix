{
  description = "Frost — a zsh-compatible shell written in Rust";

  # substrate.rust.workspace dispatches over Cargo.gen.lock (the slim gen delta,
  # reconstructed to the full BuildSpec in pure Nix) — no crate2nix, no Cargo.nix.
  inputs = {
    substrate.url = "github:pleme-io/substrate";
    # For lib.genAttrs in the frost-complete-forge secondary-package graft below.
    nixpkgs.follows = "substrate/nixpkgs";
  };

  outputs = { substrate, nixpkgs, ... }:
    let
      # The rust-workspace flake (apps/checks/devShells/overlays/packages).
      base = substrate.rust.workspace {
        src = ./.;
        member = "frost";
      };

      # `frost-complete-forge` — the completion-forge binary (crate frost-complete,
      # [[bin]] frost-complete-forge). frostmourne consumes
      # `frost.packages.<system>.frost-complete-forge` (its flake:51), but the bare
      # gen conversion (ff293c3) dropped it. Restore it as a second member build
      # grafted per-system (the c7181c4 "expose frost-complete-forge as a secondary
      # package" intent, in the gen path).
      completeBase = substrate.rust.workspace {
        src = ./.;
        member = "frost-complete";
      };
      forgeSystems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      withForge = nixpkgs.lib.genAttrs forgeSystems (system:
        (base.packages.${system} or { }) // {
          frost-complete-forge = completeBase.packages.${system}.default;
        });
    in
    # Re-attach the HM / NixOS / Darwin module trio the bare
    # `substrate.rust.workspace` shape drops. Fleet consumers (the blackmatter
    # aggregator, nix/lib/hm-modules.nix, nix/lib/nodes.nix) reference
    # `inputs.frost.{homeManagerModules,nixosModules,darwinModules}.default`, so
    # dropping them breaks the fleet's NixOS/HM eval. Each factory is a plain
    # `{ config, lib, pkgs, ... }:` module (see module/*/default.nix) — no
    # hmHelpers, no nixpkgs.lib — so the exports re-attach verbatim.
    base // {
      packages = withForge;
      homeManagerModules.default = import ./module/home-manager;
      nixosModules.default = import ./module/nixos;
      darwinModules.default = import ./module/darwin;
    };
}
