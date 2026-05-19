# frost :: nixos system module
#
# Adds frost to the system-wide `environment.systemPackages` and
# optionally registers it under `/etc/shells` so users can `chsh
# -s` to it without further setup. Per-user config still flows
# through the home-manager module — the system module is just
# "make the binary available + accept it as a login shell."
#
# Pairs with module/home-manager/default.nix for the typed
# config surface.
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.frost;
in {
  options.programs.frost = {
    enable = lib.mkEnableOption "frost — zsh-compatible Rust shell (system-wide)";

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
        Add the frost binary path to `/etc/shells` so users can
        `chsh -s $(which frost)`. NixOS handles `/etc/shells`
        generation via `environment.shells` — we just add to
        that list.
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
