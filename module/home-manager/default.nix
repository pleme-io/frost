# frost :: home-manager module
#
# Installs frost (the shell binary) AND generates a typed
# `~/.config/frost/frost.yaml` from Nix-side options that mirror
# the `FrostConfig` schema in `crates/frost-config`. Every field
# the Rust struct exposes is reachable here as a typed Nix
# option; the YAML emission preserves operator-edited content
# elsewhere via the same `home.file` semantics every other HM
# module uses.
#
# Companion to:
#   * mado/module/home-manager       — shikumi consumer #1
#   * tear/module/home-manager       — shikumi consumer (legacy LiveConfig today; migration in flight)
#   * frostmourne/module/default.nix — curated frost preset
#
# Operators get:
#   * frost binary on PATH (opt-in)
#   * frostmourne preset wired automatically when both modules
#     are enabled
#   * Live YAML config at `~/.config/frost/frost.yaml`
#   * Shikumi hot-reload (the frost runtime watches the file and
#     reloads without restart)
#   * Env-override pass-through: `FROST_PROMPT_TEMPLATE=...` etc.
#     wins at load time per shikumi's nested-env convention
#
# Dynamic configuration (the operator-facing UX):
#   * Edit ~/.config/frost/frost.yaml in any editor → frost
#     re-reads within ~250ms (debounced).
#   * Override one field via env: `FROST_HISTORY_SIZE=50000 frost`.
#   * Or replace the whole file via Nix rebuild — same hot-reload
#     path fires.
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.frost;

  # YAML rendering — pulled from a derivation so the file content
  # is determined by `cfg.settings` (the typed Nix surface) and
  # nothing else. Operators who want to edit by hand set
  # `programs.frost.manageConfig = false` and write the file
  # themselves; the binary still picks it up.
  yamlGenerator = pkgs.formats.yaml { };

  # Conditional inclusion helper — every section that has no
  # operator override drops out of the rendered YAML so we don't
  # litter the file with the literal `{}` of every default
  # subsection.
  hasAny = attrs: (builtins.length (builtins.attrNames attrs)) > 0;

  configValue =
    (if hasAny cfg.settings.options then { options = cfg.settings.options; } else {})
    // (if hasAny cfg.settings.aliases then { aliases = cfg.settings.aliases; } else {})
    // (if hasAny cfg.settings.env then { env = cfg.settings.env; } else {})
    // (if hasAny cfg.settings.keybindings then { keybindings = cfg.settings.keybindings; } else {})
    // {
      prompt = cfg.settings.prompt;
      history = cfg.settings.history;
      completion = cfg.settings.completion;
      frostmourne_preset = cfg.settings.frostmournePreset;
      reload_debounce_ms = cfg.settings.reloadDebounceMs;
    }
    // cfg.settings.extraConfig;
in {
  options.programs.frost = {
    enable = lib.mkEnableOption "frost — zsh-compatible Rust shell";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.frost or null;
      defaultText = lib.literalExpression "pkgs.frost";
      description = ''
        The frost package to install. Defaults to the flake's own
        `packages.${"\${system}"}.default`; consumers override to
        pin a specific rev.
      '';
    };

    setAsInteractiveShell = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Export `SHELL=<frost-binary>` so child processes that
        respect `$SHELL` pick frost up. Does NOT run `chsh`;
        setting the login shell on macOS requires adding the
        frost binary to `/etc/shells` first.
      '';
    };

    manageConfig = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Generate `~/.config/frost/frost.yaml` from `settings`.
        Set `false` to manage the file by hand — the binary
        still loads it; only the Nix-side write is suppressed.
      '';
    };

    configPath = lib.mkOption {
      type = lib.types.str;
      default = ".config/frost/frost.yaml";
      description = ''
        Relative path under `$HOME` for the YAML config.
        Default matches shikumi's XDG resolution; override only
        for non-standard XDG_CONFIG_HOME setups.
      '';
    };

    settings = lib.mkOption {
      description = ''
        Typed FrostConfig surface. Mirrors the Rust schema in
        `frost-config/src/lib.rs` one-to-one — every field is a
        first-class Nix option, so operators can lint by
        `darwin-rebuild` / `nixos-rebuild` instead of editor save.
      '';
      type = lib.types.submodule {
        options = {
          options = lib.mkOption {
            type = lib.types.attrsOf lib.types.bool;
            default = {};
            example = lib.literalExpression ''
              {
                EXTENDED_GLOB = true;
                GLOB_DOTS = true;
                BEEP = false;
              }
            '';
            description = ''
              Shell options — `OPTION_NAME` → bool. Mirrors zsh's
              `setopt`. Names match frost-options's `ShellOption`
              enum (case-insensitive at the runtime side).
            '';
          };

          aliases = lib.mkOption {
            type = lib.types.attrsOf lib.types.str;
            default = {};
            example = lib.literalExpression ''
              {
                ll = "ls -la";
                gst = "git status";
                k = "kubectl";
              }
            '';
            description = "Aliases — name → expansion string.";
          };

          env = lib.mkOption {
            type = lib.types.attrsOf lib.types.str;
            default = {};
            example = lib.literalExpression ''
              {
                EDITOR = "vim";
                PAGER = "less -R";
                LANG = "en_US.UTF-8";
              }
            '';
            description = ''
              Env vars set at shell startup. Use sparingly —
              `home.sessionVariables` is often the better surface
              for things every shell on the user inherits.
            '';
          };

          prompt = lib.mkOption {
            type = lib.types.submodule {
              options = {
                template = lib.mkOption {
                  type = lib.types.str;
                  default = "%n@%m %~ %# ";
                  description = "Zsh-style `%`-escape template (left prompt).";
                };
                rightTemplate = lib.mkOption {
                  type = lib.types.str;
                  default = "";
                  description = "Right-side prompt template (RPS1). Empty disables.";
                };
                subst = lib.mkOption {
                  type = lib.types.bool;
                  default = true;
                  description = "Enable command + env substitution in the prompt.";
                };
              };
            };
            default = {};
            description = "Prompt configuration.";
          };

          history = lib.mkOption {
            type = lib.types.submodule {
              options = {
                file = lib.mkOption {
                  type = lib.types.str;
                  default = "~/.local/share/frost/history";
                  description = "Path to the persistent history file (`~/` expands).";
                };
                size = lib.mkOption {
                  type = lib.types.ints.unsigned;
                  default = 10000;
                  description = "Max in-memory history entries.";
                };
                save = lib.mkOption {
                  type = lib.types.ints.unsigned;
                  default = 10000;
                  description = "Max entries persisted to disk on save.";
                };
                ignoreDups = lib.mkOption {
                  type = lib.types.bool;
                  default = true;
                  description = "Drop duplicate-of-previous entries.";
                };
                ignoreSpace = lib.mkOption {
                  type = lib.types.bool;
                  default = false;
                  description = "Drop entries whose first byte is whitespace.";
                };
              };
            };
            default = {};
            description = "History configuration.";
          };

          keybindings = lib.mkOption {
            type = lib.types.attrsOf lib.types.str;
            default = {};
            example = lib.literalExpression ''
              {
                "^A" = "beginning-of-line";
                "^E" = "end-of-line";
                "^R" = "history-incremental-search-backward";
              }
            '';
            description = "Keybindings — chord → widget name.";
          };

          completion = lib.mkOption {
            type = lib.types.submodule {
              options = {
                enabled = lib.mkOption {
                  type = lib.types.bool;
                  default = true;
                  description = "Master enable for the completion subsystem.";
                };
                ignoreCase = lib.mkOption {
                  type = lib.types.bool;
                  default = false;
                  description = "Case-insensitive matching.";
                };
                menu = lib.mkOption {
                  type = lib.types.bool;
                  default = false;
                  description = "Show menu after first Tab.";
                };
              };
            };
            default = {};
            description = "Completion configuration.";
          };

          frostmournePreset = lib.mkOption {
            type = lib.types.bool;
            default = false;
            description = ''
              Apply the frostmourne tatara-lisp preset before
              operator YAML (operator YAML still wins on
              conflict). Convenience flag for users who want
              the curated baseline without separately enabling
              `programs.frostmourne`.
            '';
          };

          reloadDebounceMs = lib.mkOption {
            type = lib.types.ints.unsigned;
            default = 250;
            description = "Notify-watcher debounce window in ms.";
          };

          extraConfig = lib.mkOption {
            type = lib.types.attrs;
            default = {};
            description = ''
              Free-form additional fields merged into the YAML
              after the typed sections. Escape hatch for keys
              added to FrostConfig after this module's last
              update; prefer adding the typed option.
            '';
          };
        };
      };
      default = {};
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = lib.optional (cfg.package != null) cfg.package;

    home.file."${cfg.configPath}" = lib.mkIf cfg.manageConfig {
      source = yamlGenerator.generate "frost.yaml" configValue;
    };

    home.sessionVariables = lib.optionalAttrs (cfg.setAsInteractiveShell && cfg.package != null) {
      SHELL = "${cfg.package}/bin/frost";
    };
  };
}
