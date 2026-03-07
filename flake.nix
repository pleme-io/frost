{
  description = "Frost — a zsh-compatible shell written in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    substrate = {
      url = "github:pleme-io/substrate";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.fenix.follows = "fenix";
    };
    devenv = {
      url = "github:cachix/devenv";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, flake-utils, substrate, devenv, ... }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ substrate.rustOverlays.${system}.rust ];
      };

      darwinBuildInputs = (import "${substrate}/lib/darwin.nix").mkDarwinBuildInputs pkgs;

      frost = pkgs.rustPlatform.buildRustPackage {
        pname = "frost";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = darwinBuildInputs;
        meta = with pkgs.lib; {
          description = "A zsh-compatible shell written in Rust";
          homepage = "https://github.com/pleme-io/frost";
          license = licenses.mit;
          mainProgram = "frost";
        };
      };
    in {
      packages = {
        default = frost;
        inherit frost;
      };

      apps.default = {
        type = "app";
        program = "${frost}/bin/frost";
      };

      devShells.default = devenv.lib.mkShell {
        inherit pkgs;
        inputs.nixpkgs = nixpkgs;
        modules = [
          ({ pkgs, ... }: {
            languages.rust = {
              enable = true;
              channel = "stable";
            };
            packages = with pkgs; [
              pkg-config
              cargo-watch
              cargo-nextest
            ] ++ darwinBuildInputs;
          })
        ];
      };
    });
}
