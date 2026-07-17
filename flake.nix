{
  description = "Frost — a zsh-compatible shell written in Rust";

  # substrate.rust.workspace dispatches over Cargo.gen.lock (the slim gen delta,
  # reconstructed to the full BuildSpec in pure Nix) — no crate2nix, no Cargo.nix.
  inputs.substrate.url = "github:pleme-io/substrate";

  outputs = { substrate, ... }:
    let
      # The rust-workspace flake (apps/checks/devShells/overlays/packages).
      base = substrate.rust.workspace {
        src = ./.;
        member = "frost";
      };
    in
    # Re-attach the HM / NixOS / Darwin module trio the bare
    # `substrate.rust.workspace` shape drops. Fleet consumers (the blackmatter
    # aggregator, nix/lib/hm-modules.nix, nix/lib/nodes.nix) reference
    # `inputs.frost.{homeManagerModules,nixosModules,darwinModules}.default`, so
    # dropping them breaks the fleet's NixOS/HM eval. Each factory is a plain
    # `{ config, lib, pkgs, ... }:` module (see module/*/default.nix) — no
    # hmHelpers, no nixpkgs.lib — so the exports re-attach verbatim, no extra
    # flake inputs required.
    base // {
      homeManagerModules.default = import ./module/home-manager;
      nixosModules.default = import ./module/nixos;
      darwinModules.default = import ./module/darwin;
    };
}
