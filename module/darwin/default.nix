# frost :: nix-darwin system module
#
# nix-darwin sibling of module/nixos/default.nix. Installs the
# frost binary under `/run/current-system/sw/bin` via
# `environment.systemPackages` so any Terminal app + every PATH
# lookup picks it up; optionally adds to `/etc/shells` for `chsh`.
#
# Per-user typed configuration still flows through the
# home-manager module — system module is just "binary on PATH".
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.frost;
in {
  options.programs.frost = {
    enable = lib.mkEnableOption "frost — zsh-compatible Rust shell (darwin system-wide)";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.frost or null;
      defaultText = lib.literalExpression "pkgs.frost";
      description = "The frost package installed system-wide.";
    };

    registerAsLoginShell = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Append the frost binary path to `/etc/shells` so users
        can `chsh -s $(which frost)`. Requires re-running
        `darwin-rebuild switch` after first enable. Operators
        still need to run `chsh -s` themselves; nix-darwin
        cannot mutate the user's shadow record automatically.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      environment.systemPackages = lib.optional (cfg.package != null) cfg.package;
    }
    (lib.mkIf cfg.registerAsLoginShell {
      environment.shells = lib.optional (cfg.package != null) "${cfg.package}/bin/frost";
    })
  ]);
}
