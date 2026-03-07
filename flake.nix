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

      cargoDeps = pkgs.rustPlatform.importCargoLock {
        lockFile = ./Cargo.lock;
      };

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

      zshSrc = pkgs.fetchFromGitHub {
        owner = "zsh-users";
        repo = "zsh";
        rev = "zsh-5.9";
        hash = "sha256-EbScbk8W4l+HVtWrQ6l01F/QnTBkVudeqG9KI578oCk=";
      };

      # Shared builder for cargo-based check derivations
      rustCheckDrv = { name, buildPhase, extraNativeBuildInputs ? [] }:
        pkgs.stdenv.mkDerivation {
          inherit name;
          src = ./.;
          inherit cargoDeps;
          nativeBuildInputs = with pkgs; [
            rustPlatform.rust.cargo
            rustPlatform.rust.rustc
            rustPlatform.cargoSetupHook
            pkg-config
          ] ++ extraNativeBuildInputs;
          buildInputs = darwinBuildInputs;
          inherit buildPhase;
          installPhase = "touch $out";
          doCheck = false;
        };

      # Script wrappers for `nix run .#<name>` apps
      mkApp = name: script: {
        type = "app";
        program = toString (pkgs.writeShellScript "frost-${name}" script);
      };

    in {
      packages = {
        default = frost;
        inherit frost;
      };

      apps = {
        # nix run — launch frost shell
        default = {
          type = "app";
          program = "${frost}/bin/frost";
        };

        # nix run .#test — run cargo test
        test = mkApp "test" ''
          set -euo pipefail
          echo "=== frost: cargo test ==="
          cargo test --workspace 2>&1
          echo "✓ all tests passed"
        '';

        # nix run .#clippy — run cargo clippy
        clippy = mkApp "clippy" ''
          set -euo pipefail
          echo "=== frost: cargo clippy ==="
          cargo clippy --workspace -- -D warnings 2>&1
          echo "✓ clippy clean"
        '';

        # nix run .#fmt — run cargo fmt --check
        fmt = mkApp "fmt" ''
          set -euo pipefail
          echo "=== frost: cargo fmt --check ==="
          cargo fmt --check --all 2>&1
          echo "✓ formatting ok"
        '';

        # nix run .#compat — run zsh compatibility test suite
        compat = mkApp "compat" ''
          set -euo pipefail
          echo "=== frost: zsh compat tests ==="
          echo "zsh test suite: ${zshSrc}/Test"
          if command -v frost-compat >/dev/null 2>&1; then
            frost-compat --frost ${frost}/bin/frost --verbose "${zshSrc}/Test"
          elif [ -f target/debug/frost-compat ]; then
            target/debug/frost-compat --frost ${frost}/bin/frost --verbose "${zshSrc}/Test"
          else
            echo "building frost-compat..."
            cargo build -p frost-compat 2>/dev/null
            target/debug/frost-compat --frost ${frost}/bin/frost --verbose "${zshSrc}/Test"
          fi
        '';

        # nix run .#ci — run all checks sequentially (test + clippy + fmt)
        ci = mkApp "ci" ''
          set -euo pipefail
          echo "╔════════════════════════════════╗"
          echo "║     frost CI pipeline          ║"
          echo "╚════════════════════════════════╝"
          echo ""

          echo "── [1/3] cargo test ──"
          cargo test --workspace 2>&1
          echo ""

          echo "── [2/3] cargo clippy ──"
          cargo clippy --workspace -- -D warnings 2>&1
          echo ""

          echo "── [3/3] cargo fmt --check ──"
          cargo fmt --check --all 2>&1
          echo ""

          echo "✓ all checks passed"
        '';
      };

      # nix flake check — pure nix-sandbox checks
      checks = {
        build = frost;

        test = rustCheckDrv {
          name = "frost-test";
          buildPhase = "cargo test --workspace";
        };

        clippy = rustCheckDrv {
          name = "frost-clippy";
          extraNativeBuildInputs = [ pkgs.clippy ];
          buildPhase = "cargo clippy --workspace -- -D warnings 2>&1";
        };

        fmt = rustCheckDrv {
          name = "frost-fmt";
          extraNativeBuildInputs = [ pkgs.rustfmt ];
          buildPhase = "cargo fmt --check --all 2>&1";
        };
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
