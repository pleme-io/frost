;; frostmourne :: 00-core
;; ─────────────────────
;; Aliases, shell options, and environment defaults that everyone
;; re-invents by hand. Lexical prefix 00 = loads first.

;; ── Navigation + listing ──────────────────────────────────────────
(defalias :name "l"    :value "ls")
(defalias :name "la"   :value "ls -A")
(defalias :name "ll"   :value "ls -la")
(defalias :name "lt"   :value "ls -la --color=always")
(defalias :name ".."   :value "cd ..")
(defalias :name "..."  :value "cd ../..")
(defalias :name "...." :value "cd ../../..")
(defalias :name "-"    :value "cd -")

;; ── Git ───────────────────────────────────────────────────────────
(defalias :name "g"    :value "git")
(defalias :name "ga"   :value "git add")
(defalias :name "gc"   :value "git commit")
(defalias :name "gco"  :value "git checkout")
(defalias :name "gd"   :value "git diff")
(defalias :name "gl"   :value "git log --oneline --graph --decorate")
(defalias :name "gp"   :value "git push")
(defalias :name "gs"   :value "git status")
(defalias :name "gst"  :value "git status -sb")
(defalias :name "gpl"  :value "git pull")

;; ── Containers / orchestration ────────────────────────────────────
(defalias :name "d"    :value "docker")
(defalias :name "dc"   :value "docker compose")
(defalias :name "k"    :value "kubectl")
(defalias :name "kgp"  :value "kubectl get pods")
(defalias :name "kgs"  :value "kubectl get svc")

;; ── Infra / build ─────────────────────────────────────────────────
(defalias :name "tf"   :value "terraform")
(defalias :name "c"    :value "cargo")
(defalias :name "cb"   :value "cargo build")
(defalias :name "ct"   :value "cargo test")
(defalias :name "cr"   :value "cargo run")
(defalias :name "nr"   :value "nix run")
(defalias :name "nd"   :value "nix develop")

;; ── Daily ergonomics ──────────────────────────────────────────────
(defalias :name "mkd"  :value "mkdir -p")
(defalias :name "cls"  :value "clear")
(defalias :name "h"    :value "history")
(defalias :name "j"    :value "jobs -l")
(defalias :name "path" :value "echo $PATH | tr : '\\n'")

;; ── Shell options ─────────────────────────────────────────────────
;; Enable the features a modern shell assumes; turn off the annoying bell.
(defopts :enable ("extendedglob" "globdots" "promptsubst"
                  "histignoredups" "histignorespace"
                  "interactivecomments" "autocd")
         :disable ("beep"))

;; ── Environment ───────────────────────────────────────────────────
(defenv :name "EDITOR"    :value "blnvim"      :export #t)
(defenv :name "VISUAL"    :value "blnvim"      :export #t)
(defenv :name "PAGER"     :value "less -R"     :export #t)
(defenv :name "LESS"      :value "-R --mouse"  :export #t)
(defenv :name "LANG"      :value "en_US.UTF-8" :export #t)
(defenv :name "LC_ALL"    :value "en_US.UTF-8" :export #t)
(defenv :name "CLICOLOR"  :value "1"           :export #t)
(defenv :name "TERM_COLOR_MODE" :value "truecolor")

;; frostmourne :: 10-prompt
;; ────────────────────────
;; Two-line prompt, all information driven by vars the hooks in
;; 20-hooks.lisp populate each cycle. `prompt_subst` is on so `$…`
;; references expand at render time.
;;
;; Top line:  user@host in cwd ⎇ branch*  3s
;; Status:    ╰─ ✓  or  ╰─ ✗ 1
;;
;; Colors: 244 is a muted gray for decoration, green/blue/cyan for the
;; context triad, red for error states. Every `%F{…}` closes with `%f`
;; so SGR state can't leak past the prompt.

(defprompt
  :ps1 "
%F{244}╭─%f %F{green}%n%f@%F{blue}%m%f %F{244}in%f %F{cyan}%~%f%F{244}${FROST_GIT_BRANCH}%f%F{yellow}${FROST_CMD_DURATION}%f
%F{244}╰─%f ${FROST_LAST_STATUS_GLYPH}%F{244}:%f "
  :ps2 "%F{244}  %f"
  :prompt-subst #t)

;; frostmourne :: 20-hooks
;; ---------------------
;; preexec stamps the start time of each command.
;; precmd  derives duration, git branch/dirty marker, last-status glyph.
;; chpwd   clears the cached duration after a bare `cd`.
;;
;; NOTE: shell `#` line comments inside a Lisp `:body "..."` string trip
;; tatara-lisp's parser, so bodies use no `#` comments. Leading-colon
;; `:` null-commands are the zsh-idiomatic alternative when a body needs
;; an inline reminder.

(defhook
  :event "preexec"
  :body "FROST_CMD_START=$(date +%s)")

(defhook
  :event "precmd"
  :body "
rc=$?
if [ -n \"$FROST_CMD_START\" ]; then
  now=$(date +%s)
  dur=$((now - FROST_CMD_START))
  if [ \"$dur\" -ge 1 ]; then
    FROST_CMD_DURATION=\" ${dur}s\"
  else
    FROST_CMD_DURATION=\"\"
  fi
  unset FROST_CMD_START
else
  FROST_CMD_DURATION=\"\"
fi
export FROST_CMD_DURATION
branch=$(git branch --show-current 2>/dev/null)
if [ -n \"$branch\" ]; then
  dirty=\"\"
  if [ -n \"$(git status --porcelain 2>/dev/null)\" ]; then
    dirty=\"*\"
  fi
  FROST_GIT_BRANCH=\" ⎇ ${branch}${dirty}\"
else
  FROST_GIT_BRANCH=\"\"
fi
export FROST_GIT_BRANCH
if [ \"$rc\" -eq 0 ]; then
  FROST_LAST_STATUS_GLYPH=\"✓\"
else
  FROST_LAST_STATUS_GLYPH=\"✗ ${rc}\"
fi
export FROST_LAST_STATUS_GLYPH")

(defhook
  :event "chpwd"
  :body "FROST_CMD_DURATION=\"\"; export FROST_CMD_DURATION")

;; frostmourne :: 30-bindings
;; ──────────────────────────
;; Authoring-only today — the ZLE dispatcher that consults
;; `__frost_bind_<CANON_KEY>` in env.functions is queued for a
;; follow-up. These bindings describe the intent that lands the moment
;; the dispatcher wires up.

(defbind :key "C-x e" :action "exec $EDITOR")
(defbind :key "C-l"   :action "clear")
(defbind :key "M-?"   :action "help")

;; frostmourne :: 40-completions
;; ─────────────────────────────
;; Each `defcompletion` stores a JSON payload in
;; `__frost_complete_<command>` that frost-complete will consume when
;; its dispatcher lands. Until then, these forms validate at load time
;; and act as living documentation of the curated command vocabulary.

(defcompletion
  :command "git"
  :args ("status" "diff" "log" "commit" "push" "pull" "fetch" "rebase"
         "checkout" "branch" "merge" "stash" "reset" "restore" "show"
         "add" "rm" "mv" "tag" "remote" "clone" "init" "blame")
  :description "version control")

(defcompletion
  :command "kubectl"
  :args ("get" "describe" "apply" "delete" "logs" "exec" "port-forward"
         "rollout" "scale" "create" "edit" "config" "cluster-info"
         "top" "drain" "cordon" "uncordon")
  :description "Kubernetes CLI")

(defcompletion
  :command "docker"
  :args ("run" "exec" "build" "pull" "push" "ps" "logs" "compose"
         "images" "rm" "rmi" "inspect" "network" "volume" "stats")
  :description "container runtime")

(defcompletion
  :command "cargo"
  :args ("build" "test" "run" "check" "clippy" "fmt" "doc" "publish"
         "new" "init" "add" "remove" "update" "install" "search"
         "tree" "metadata" "bench")
  :description "Rust package manager")

(defcompletion
  :command "nix"
  :args ("build" "run" "develop" "shell" "flake" "profile" "store"
         "eval" "log" "copy" "registry" "search" "hash" "why-depends")
  :description "Nix CLI")

(defcompletion
  :command "frost"
  :args ("-c" "--command" "-i" "--interactive" "-V" "--version" "-h" "--help")
  :description "the Rust zsh-replacement")

;; frostmourne :: 50-functions
;; ───────────────────────────
;; Small utility functions that feel like builtins.

;; mkcd <dir> — mkdir + cd in one go.
(defun :name "mkcd"
       :body "mkdir -p \"$1\" && cd \"$1\"")

;; up [N] — cd N directories up (default 1).
(defun :name "up"
       :body "cd $(printf '../%.0s' $(seq 1 ${1:-1}))")

;; extract <file> — auto-detect archive type and unpack.
(defun :name "extract"
       :body "
case \"$1\" in
  *.tar.gz|*.tgz)  tar xzf \"$1\" ;;
  *.tar.bz2|*.tbz) tar xjf \"$1\" ;;
  *.tar.xz)        tar xJf \"$1\" ;;
  *.tar)           tar xf \"$1\"  ;;
  *.zip)           unzip \"$1\"   ;;
  *.7z)            7z x \"$1\"    ;;
  *)               echo \"extract: unknown archive type: $1\" >&2 ; return 1 ;;
esac")

;; reload — re-source the shell's Lisp rc file (picks up edits without
;; starting a new frost process; works because the rc applies to the
;; live env).
(defun :name "reload"
       :body "frost -c \"echo 'reload is a stub — restart frost for now'\"")

;; frostmourne :: 60-tools-zoxide
;; ------------------------------
;; `z <partial>` jumps to the best-matching directory; `zi` opens an
;; interactive picker. Recipe installs both aliases + the chpwd hook
;; that records every directory change into zoxide's database.
(defintegration :tool "zoxide")

;; frostmourne :: 61-tools-skim
;; ----------------------------
;; skim (Rust fuzzy finder, binary: `sk`) + skim-tab integrations.
;; Mirrors the widget stack from `blackmatter-shell`'s
;; `skim-rs/skim/config/init.zsh` — four pickers bound to four keys,
;; authored declaratively here and dispatched by frost's REPL via the
;; `__frost_picker_*__` sentinels.
;;
;;   C-r  →  history picker     (replace buffer, user reviews + Enter)
;;   C-t  →  file picker        (append selection at cursor)
;;   M-c  →  cd picker          (becomes `cd <dir>` and auto-submits)
;;   C-f  →  content picker     (rg → skim → `$EDITOR +line path`, auto-submit)
;;
;; Each sentinel is intercepted pre-exec in `frost/src/main.rs ::
;; dispatch_picker_sentinel` — it is never parsed or executed as a
;; regular shell command.

;; ── Environment: Nord-themed skim by default ────────────────────────
;; SKIM_DEFAULT_OPTIONS is read by `sk` on every invocation. Users can
;; override in `~/.frostrc.lisp` with a later `(defenv …)` — last-wins.
(defenv
  :name "SKIM_DEFAULT_OPTIONS"
  :value "--height=40% --layout=reverse --ansi --prompt=> --color=fg:#D8DEE9,bg:#2E3440,hl:#88C0D0:bold:underlined,fg+:#ECEFF4:bold,bg+:#3B4252,hl+:#8FBCBB:bold:underlined,info:#4C566A,prompt:#A3BE8C,pointer:#88C0D0,marker:#B48EAD,spinner:#81A1C1,header:#5E81AC,border:#4C566A,query:#ECEFF4:bold"
  :export #t)
;; Seed Ctrl-T's stock skim binding (if the user keeps skim's own
;; key-bindings.zsh loaded in parallel) with the same source we use in
;; the picker — `fd` over the cwd, hidden files on, .git excluded.
(defenv
  :name "SKIM_DEFAULT_COMMAND"
  :value "fd --type f --hidden --follow --exclude .git"
  :export #t)
(defenv
  :name "SKIM_CTRL_T_COMMAND"
  :value "fd --type f --hidden --follow --exclude .git"
  :export #t)
(defenv
  :name "SKIM_ALT_C_COMMAND"
  :value "fd --type d --hidden --follow --exclude .git"
  :export #t)

;; ── Aliases: ergonomic shortcuts to the skim-tab binary set ─────────
;; skim-tab ships 9 pickers. The 4 bound to keys above are the
;; REPL-integrated ones; the others are useful standalone — surface
;; them here so `fvim`, `fco`, `fkill`, `kpod` Just Work out of the box.
(defalias :name "fh"     :value "skim-history")     ; history picker (plain stdout)
(defalias :name "ff"     :value "skim-files")       ; file picker
(defalias :name "fd-"    :value "skim-cd")          ; cd picker (paths are shell-quoted)
(defalias :name "fg"     :value "skim-content")     ; content search
(defalias :name "fvim"   :value "skim-fvim")        ; open file in $EDITOR
(defalias :name "fco"    :value "skim-fco")         ; git checkout picker
(defalias :name "fkill"  :value "skim-fkill")       ; kill picker
(defalias :name "kpod"   :value "skim-kpod")        ; k8s pod selector

;; ── Pickers: the four skim-rs widgets, declaratively ───────────────
;; `defpicker` is the grown-up form for binding keys to terminal-
;; takeover pickers. It encodes the full convention in one place:
;;   :name   — widget short name; becomes sentinel __frost_picker_NAME__
;;   :key    — chord (parsed by frost-zle::parse_chord)
;;   :binary — what to spawn; must be on $PATH when the key fires
;;   :action — how to consume the selection:
;;       "replace"   — selection replaces buffer; user reviews + Enter
;;       "append"    — selection appends to LBUFFER (with separator)
;;       "cd-submit" — buffer becomes `cd <sel>` and auto-submits
;;       "submit"    — selection is the command verbatim; auto-submits
;;
;; The REPL sees the sentinel directly (no defbind indirection) and
;; dispatches to :binary with the live LBUFFER forwarded as `--query`.
(defpicker :name "history" :key "C-r" :binary "skim-history" :action "replace")
(defpicker :name "files"   :key "C-t" :binary "skim-files"   :action "append")
(defpicker :name "cd"      :key "M-c" :binary "skim-cd"      :action "cd-submit")
(defpicker :name "content" :key "C-f" :binary "skim-content" :action "submit")

;; frostmourne :: 62-tools-direnv
;; ------------------------------
;; Automatic `.envrc` loading on directory change via the built-in
;; direnv integration. One line; the recipe in
;; frost-lisp/src/integration.rs installs the chpwd hook that
;; captures `direnv export` output.
(defintegration :tool "direnv")

;; frostmourne :: 63-tools-starship
;; --------------------------------
;; starship prompt integration. `defprompt :command` synthesizes a
;; `precmd` hook that captures `starship prompt`'s stdout into PS1
;; every prompt — the cleanest declarative hook-up we have.
;;
;; Disabled by default: the frost-native prompt in 10-prompt.lisp +
;; 20-hooks.lisp already gives us a two-line git/duration/status-aware
;; prompt with Nord coloring, and avoids a subprocess per prompt. If
;; you prefer starship's renderer (modules, custom segments, toml
;; config in ~/.config/starship.toml), delete 10-prompt.lisp from
;; your layer and uncomment the form below. Last prompt form wins.
;;
;; (defprompt
;;   :command "starship prompt --status=\"$?\" --cmd-duration=\"${FROST_CMD_DURATION_MS:-0}\"")

;; frostmourne :: 64-tools-atuin
;; -----------------------------
;; atuin history aliases + ATUIN_NOBIND export. One-liner via the
;; built-in integration; the recipe defines `h`/`hist-stats`/
;; `hist-import` and sets ATUIN_NOBIND so atuin's own default keybind
;; doesn't clash with the skim-history picker bound in 61-tools-skim.
(defintegration :tool "atuin")

;; frostmourne :: 65-tools-modern-unix
;; -----------------------------------
;; Opt-in replacements for classic Unix tools. Enabling happens via
;; alias so original binaries stay reachable (`\cat`, `\ls`, `\grep`).

(defalias :name "cat"  :value "bat --plain --paging=never")
(defalias :name "less" :value "bat --paging=always")
(defalias :name "grep" :value "rg")
(defalias :name "find" :value "fd")

;; eza > ls — show git status + icons when available.
(defalias :name "ls"   :value "eza --git")
(defalias :name "ll"   :value "eza -la --git --group-directories-first")
(defalias :name "la"   :value "eza -A --git")
(defalias :name "lt"   :value "eza -la --git --tree --level=2 --group-directories-first")

;; delta makes git diff legible.
(defenv :name "GIT_PAGER" :value "delta" :export #t)

;; frostmourne :: 66-tools-utility
;; -------------------------------
;; Rust-native utility tier bundled by frostmourne's flake.
;; blzsh-parity: every tool below is a pure-Rust classic-unix
;; replacement or observability aid. Aliases surface short names so
;; they're discoverable at the prompt.

;; ── procs: ps replacement ───────────────────────────────────────────
(defalias :name "ps-"    :value "procs")       ; avoid clobbering system ps
(defalias :name "psa"    :value "procs --tree")
(defalias :name "pst"    :value "procs --tree --sortd cpu")

;; ── sd: sed replacement (simpler regex swap) ────────────────────────
;; no alias — `sd pattern replacement <file>` is the full UX and
;; aliasing to `sed` would break scripts that expect POSIX sed.

;; ── tokei: codebase stats ───────────────────────────────────────────
(defalias :name "loc"    :value "tokei")
(defalias :name "loc-j"  :value "tokei --output json")

;; ── hyperfine: benchmarking ─────────────────────────────────────────
(defalias :name "bench"  :value "hyperfine --warmup 3")

;; ── tealdeer: tldr client ───────────────────────────────────────────
;; expose as both `tldr` (the canonical command) and the fuller name.
(defalias :name "tldr"   :value "tldr")
(defalias :name "help-"  :value "tldr")

;; ── bandwhich: per-process bandwidth ────────────────────────────────
;; sudo is usually required on Linux/macOS for raw socket access; keep
;; the alias minimal so users can decide when to sudo.
(defalias :name "bw"     :value "bandwhich")

;; ── grex: regex generator from examples ─────────────────────────────
;; `grex foo bar baz` → the least-common regex matching all three.
(defalias :name "regex"  :value "grex")

;; ── shfmt: shell script formatter ───────────────────────────────────
;; `-i 2` = 2-space indent, `-ci` = indent case labels, `-bn` = line-
;; break before `&&` / `||`. Matches the blzsh default style.
(defalias :name "shfmt-" :value "shfmt -i 2 -ci -bn")

;; frostmourne :: 70-tools-kubernetes
;; ----------------------------------
;; kubectl / helm / flux / kubectx / kubens / k9s / kubecolor daily-ops
;; shortcuts. Mirrors the blzsh kubernetes-tools set and
;; blackmatter-kubernetes' alias layout.
;;
;; Tool bundle (see flake.nix bundledTools):
;;   kubectl        — cluster CLI
;;   kubecolor      — colorized kubectl wrapper (bound to `k` + `kc`)
;;   kubectx        — switch contexts
;;   kubens         — switch namespaces
;;   helm           — charts
;;   flux           — GitOps
;;   k9s            — TUI
;;   k3d            — local clusters
;;   kind           — local clusters (alt)
;;   stern          — multi-pod log tailing

;; ── Primary wrapper ────────────────────────────────────────────────
(defalias :name "k"       :value "kubecolor")
(defalias :name "kc"      :value "kubecolor")
(defalias :name "kctl"    :value "kubectl")  ; bypass kubecolor for pipes

;; ── Context / namespace switching ──────────────────────────────────
(defalias :name "kcx"     :value "kubectx")
(defalias :name "kns"     :value "kubens")
(defalias :name "kcurr"   :value "kubectl config current-context")
(defalias :name "kctx"    :value "kubectx")   ; alias often used in blzsh

;; ── Resource listing (kubecolor-aware) ─────────────────────────────
(defalias :name "kpods"   :value "kubecolor get pods")
(defalias :name "ksvc"    :value "kubecolor get svc")
(defalias :name "kdep"    :value "kubecolor get deploy")
(defalias :name "ksts"    :value "kubecolor get sts")
(defalias :name "knode"   :value "kubecolor get nodes")
(defalias :name "kns-"    :value "kubecolor get ns")
(defalias :name "kev"     :value "kubecolor get events --sort-by=.lastTimestamp")
(defalias :name "ka"      :value "kubecolor get all -A")
(defalias :name "kall"    :value "kubecolor get all")

;; ── Inspection ─────────────────────────────────────────────────────
(defalias :name "kd"      :value "kubecolor describe")
(defalias :name "kdp"     :value "kubecolor describe pod")
(defalias :name "kds"     :value "kubecolor describe svc")

;; ── Logs / exec ────────────────────────────────────────────────────
(defalias :name "klog"    :value "kubecolor logs")
(defalias :name "klogf"   :value "kubecolor logs -f")
(defalias :name "ksh"     :value "kubectl exec -it")
(defalias :name "kstern"  :value "stern")

;; ── Apply / delete ─────────────────────────────────────────────────
(defalias :name "kaf"     :value "kubectl apply -f")
(defalias :name "kdel"    :value "kubectl delete")
(defalias :name "kdelf"   :value "kubectl delete -f")

;; ── Helm ───────────────────────────────────────────────────────────
(defalias :name "h"       :value "helm")
(defalias :name "hi"      :value "helm install")
(defalias :name "hu"      :value "helm upgrade")
(defalias :name "hd"      :value "helm delete")
(defalias :name "hls"     :value "helm ls -A")

;; ── Flux ───────────────────────────────────────────────────────────
(defalias :name "f"       :value "flux")
(defalias :name "fg"      :value "flux get")
(defalias :name "fgk"     :value "flux get kustomizations")
(defalias :name "fgh"     :value "flux get helmreleases")
(defalias :name "freq"    :value "flux reconcile")
(defalias :name "fres"    :value "flux suspend")
(defalias :name "ferum"   :value "flux resume")

;; ── k9s / local clusters ───────────────────────────────────────────
(defalias :name "k9"      :value "k9s")
(defalias :name "k3"      :value "k3d")

;; ── Completions the skim-tab picker + frost-complete can drive ─────
(defcompletion
  :command "k"
  :args ("apply" "get" "describe" "delete" "edit" "exec" "logs" "rollout" "scale" "explain" "cluster-info" "config" "patch" "label" "annotate" "port-forward" "cp" "top" "wait" "diff" "kustomize")
  :description "kubectl (via kubecolor)")
(defcompletion
  :command "kubectl"
  :args ("apply" "get" "describe" "delete" "edit" "exec" "logs" "rollout" "scale" "explain" "cluster-info" "config" "patch" "label" "annotate" "port-forward" "cp" "top" "wait" "diff" "kustomize")
  :description "kubernetes CLI")
(defcompletion
  :command "helm"
  :args ("install" "upgrade" "uninstall" "list" "ls" "repo" "search" "show" "template" "get" "rollback" "status" "history" "test" "lint" "dependency" "pull" "package" "push")
  :description "kubernetes package manager")
(defcompletion
  :command "flux"
  :args ("get" "create" "delete" "reconcile" "resume" "suspend" "check" "export" "uninstall" "install" "bootstrap" "diff" "logs" "stats" "events" "trace" "version")
  :description "GitOps toolkit")

;; frostmourne :: 71-tools-git
;; ---------------------------
;; Git daily-ops shortcuts. Matches blzsh's zsh-users/zsh-git alias
;; conventions where those exist, plus pleme-io additions (delta
;; already wired in 65-tools-modern-unix as GIT_PAGER).
;;
;; The set below intentionally overlaps heavily with `oh-my-zsh`'s
;; git plugin so muscle memory transfers — no point reinventing
;; `gst`, `gco`, `gcm`.

;; ── Primary ────────────────────────────────────────────────────────
(defalias :name "g"       :value "git")

;; ── Status / info ──────────────────────────────────────────────────
(defalias :name "gs"      :value "git status -sb")
(defalias :name "gst"     :value "git status")
(defalias :name "gb"      :value "git branch")
(defalias :name "gba"     :value "git branch -a")
(defalias :name "gbd"     :value "git branch -d")

;; ── Diff / log (delta pipes via GIT_PAGER from 65-tools-modern-unix) ─
(defalias :name "gd"      :value "git diff")
(defalias :name "gds"     :value "git diff --staged")
(defalias :name "gl"      :value "git log --oneline --graph --decorate -n 20")
(defalias :name "gla"     :value "git log --oneline --graph --decorate --all -n 40")
(defalias :name "gll"     :value "git log -n 20")

;; ── Add / commit ───────────────────────────────────────────────────
(defalias :name "ga"      :value "git add")
(defalias :name "gap"     :value "git add -p")
(defalias :name "gaa"     :value "git add -A")
(defalias :name "gc"      :value "git commit")
(defalias :name "gcm"     :value "git commit -m")
(defalias :name "gca"     :value "git commit --amend")
(defalias :name "gcan"    :value "git commit --amend --no-edit")

;; ── Checkout / switch ──────────────────────────────────────────────
(defalias :name "gco"     :value "git checkout")
(defalias :name "gcob"    :value "git checkout -b")
(defalias :name "gsw"     :value "git switch")
(defalias :name "gswc"    :value "git switch -c")

;; ── Pull / push / fetch ────────────────────────────────────────────
(defalias :name "gp"      :value "git push")
(defalias :name "gpf"     :value "git push --force-with-lease")
(defalias :name "gpu"     :value "git push -u origin HEAD")
(defalias :name "gpl"     :value "git pull")
(defalias :name "gplr"    :value "git pull --rebase")
(defalias :name "gf"      :value "git fetch")
(defalias :name "gfa"     :value "git fetch --all --prune")

;; ── Stash ──────────────────────────────────────────────────────────
(defalias :name "gsta"    :value "git stash")
(defalias :name "gstp"    :value "git stash pop")
(defalias :name "gstl"    :value "git stash list")

;; ── Rebase ─────────────────────────────────────────────────────────
(defalias :name "grb"     :value "git rebase")
(defalias :name "grbm"    :value "git rebase main")
(defalias :name "grbi"    :value "git rebase -i")
(defalias :name "grbc"    :value "git rebase --continue")
(defalias :name "grba"    :value "git rebase --abort")

;; ── Reset / restore ────────────────────────────────────────────────
(defalias :name "grst"    :value "git reset")
(defalias :name "grsth"   :value "git reset --hard")
(defalias :name "grs"     :value "git restore")

;; ── Remote ─────────────────────────────────────────────────────────
(defalias :name "gr"      :value "git remote -v")
(defalias :name "gra"     :value "git remote add")

;; ── Housekeeping ───────────────────────────────────────────────────
(defalias :name "gclean"  :value "git clean -fd")
(defalias :name "gprune"  :value "git remote prune origin")

;; ── tig (if present — optional TUI) ────────────────────────────────
(defalias :name "t"       :value "tig")

;; ── Completions ────────────────────────────────────────────────────
(defcompletion
  :command "git"
  :args ("status" "add" "commit" "push" "pull" "fetch" "merge" "rebase" "checkout" "switch" "branch" "log" "diff" "stash" "reset" "restore" "clone" "remote" "tag" "show" "cherry-pick" "bisect" "blame" "reflog" "clean" "worktree" "submodule" "config" "init" "apply" "revert")
  :description "version control")

;; frostmourne :: 72-tools-cloud
;; -----------------------------
;; Cloud CLI shortcuts. aws / gcloud / az don't ship universal
;; "default profile" or "most-used subcommand" behaviors, so this
;; file is thinner than the k8s one — we mostly expose sensible
;; defaults + completion specs.

;; ── AWS ────────────────────────────────────────────────────────────
(defalias :name "a"       :value "aws")
(defalias :name "awho"    :value "aws sts get-caller-identity")
(defalias :name "aec2"    :value "aws ec2")
(defalias :name "as3"     :value "aws s3")
(defalias :name "aeks"    :value "aws eks")
(defalias :name "alog"    :value "aws logs")

;; ── GCP ────────────────────────────────────────────────────────────
(defalias :name "gc"      :value "gcloud")
(defalias :name "gwho"    :value "gcloud auth list")
(defalias :name "gproj"   :value "gcloud config set project")
(defalias :name "ggke"    :value "gcloud container clusters")

;; ── Azure ──────────────────────────────────────────────────────────
(defalias :name "az-"     :value "az")   ; avoid clobbering `az` if aliased
(defalias :name "azwho"   :value "az account show")

;; ── Completion specs ──────────────────────────────────────────────
(defcompletion
  :command "aws"
  :args ("s3" "ec2" "iam" "eks" "ecs" "lambda" "logs" "rds" "sts" "ssm" "cloudformation" "cloudwatch" "route53" "dynamodb" "secretsmanager" "sns" "sqs" "kms" "sts")
  :description "AWS CLI")
(defcompletion
  :command "gcloud"
  :args ("compute" "container" "iam" "storage" "auth" "config" "functions" "pubsub" "sql" "services" "projects" "logging" "kms" "secrets" "builds" "run" "dataflow" "monitoring")
  :description "Google Cloud CLI")
(defcompletion
  :command "az"
  :args ("login" "account" "group" "vm" "storage" "keyvault" "aks" "acr" "network" "functionapp" "webapp" "monitor" "policy" "role" "appconfig" "cosmosdb" "sql")
  :description "Azure CLI")


;; frostmourne :: skim-tab-generated completions
;; Regenerate by bumping the skim-tab input — do not hand-edit.

;; subcommands (auto-generated)
(defsubcmd :path "aws" :name "apigateway" :description "API management")
(defsubcmd :path "aws" :name "autoscaling" :description "Auto scaling")
(defsubcmd :path "aws" :name "cloudformation" :description "Infrastructure as code")
(defsubcmd :path "aws" :name "cloudwatch" :description "Monitoring & logs")
(defsubcmd :path "aws" :name "codebuild" :description "Build service")
(defsubcmd :path "aws" :name "codecommit" :description "Git hosting")
(defsubcmd :path "aws" :name "codepipeline" :description "CI/CD pipeline")
(defsubcmd :path "aws" :name "dynamodb" :description "NoSQL database")
(defsubcmd :path "aws" :name "ec2" :description "Virtual machines")
(defsubcmd :path "aws" :name "ecr" :description "Container registry")
(defsubcmd :path "aws" :name "ecs" :description "Container service")
(defsubcmd :path "aws" :name "eks" :description "Kubernetes service")
(defsubcmd :path "aws" :name "elb" :description "Load balancing")
(defsubcmd :path "aws" :name "iam" :description "Identity & access")
(defsubcmd :path "aws" :name "kms" :description "Key management")
(defsubcmd :path "aws" :name "lambda" :description "Serverless functions")
(defsubcmd :path "aws" :name "logs" :description "CloudWatch logs")
(defsubcmd :path "aws" :name "rds" :description "Relational databases")
(defsubcmd :path "aws" :name "route53" :description "DNS management")
(defsubcmd :path "aws" :name "s3" :description "Object storage")
(defsubcmd :path "aws" :name "secretsmanager" :description "Secret storage")
(defsubcmd :path "aws" :name "sns" :description "Pub/sub notifications")
(defsubcmd :path "aws" :name "sqs" :description "Message queues")
(defsubcmd :path "aws" :name "ssm" :description "Systems manager")
(defsubcmd :path "aws" :name "sts" :description "Security tokens")
(defsubcmd :path "az" :name "account" :description "Subscriptions")
(defsubcmd :path "az" :name "acr" :description "Container Registry")
(defsubcmd :path "az" :name "ad" :description "Azure AD")
(defsubcmd :path "az" :name "aks" :description "Kubernetes Service")
(defsubcmd :path "az" :name "appservice" :description "App Service")
(defsubcmd :path "az" :name "cognitiveservices" :description "AI services")
(defsubcmd :path "az" :name "container" :description "Container Instances")
(defsubcmd :path "az" :name "cosmosdb" :description "Cosmos DB")
(defsubcmd :path "az" :name "eventhubs" :description "Event Hubs")
(defsubcmd :path "az" :name "functionapp" :description "Function Apps")
(defsubcmd :path "az" :name "group" :description "Resource groups")
(defsubcmd :path "az" :name "identity" :description "Managed identities")
(defsubcmd :path "az" :name "iot" :description "IoT Hub")
(defsubcmd :path "az" :name "keyvault" :description "Key Vault")
(defsubcmd :path "az" :name "monitor" :description "Monitoring")
(defsubcmd :path "az" :name "network" :description "Virtual networks")
(defsubcmd :path "az" :name "policy" :description "Azure Policy")
(defsubcmd :path "az" :name "resource" :description "Resources")
(defsubcmd :path "az" :name "role" :description "Role assignments")
(defsubcmd :path "az" :name "servicebus" :description "Service Bus")
(defsubcmd :path "az" :name "sql" :description "SQL databases")
(defsubcmd :path "az" :name "storage" :description "Storage accounts")
(defsubcmd :path "az" :name "vm" :description "Virtual machines")
(defsubcmd :path "az" :name "webapp" :description "Web Apps")
(defsubcmd :path "cargo" :name "add" :description "Add dependency to Cargo.toml")
(defsubcmd :path "cargo" :name "bench" :description "Run benchmarks")
(defsubcmd :path "cargo" :name "build" :description "Compile the current package")
(defsubcmd :path "cargo" :name "check" :description "Check without building")
(defsubcmd :path "cargo" :name "clean" :description "Remove build artifacts")
(defsubcmd :path "cargo" :name "clippy" :description "Run Clippy lints")
(defsubcmd :path "cargo" :name "doc" :description "Build documentation")
(defsubcmd :path "cargo" :name "fmt" :description "Format source code")
(defsubcmd :path "cargo" :name "init" :description "Create new package in existing dir")
(defsubcmd :path "cargo" :name "install" :description "Install a Rust binary")
(defsubcmd :path "cargo" :name "new" :description "Create a new Cargo package")
(defsubcmd :path "cargo" :name "nextest" :description "Run tests via cargo-nextest")
(defsubcmd :path "cargo" :name "publish" :description "Publish to crates.io")
(defsubcmd :path "cargo" :name "remove" :description "Remove a dependency")
(defsubcmd :path "cargo" :name "run" :description "Run a binary or example")
(defsubcmd :path "cargo" :name "test" :description "Run the tests")
(defsubcmd :path "cargo" :name "tree" :description "Display dependency tree")
(defsubcmd :path "cargo" :name "update" :description "Update dependencies")
(defsubcmd :path "docker" :name "build" :description "Build an image")
(defsubcmd :path "docker" :name "compose" :description "Docker Compose")
(defsubcmd :path "docker" :name "exec" :description "Execute in container")
(defsubcmd :path "docker" :name "images" :description "List images")
(defsubcmd :path "docker" :name "logs" :description "Container logs")
(defsubcmd :path "docker" :name "ps" :description "List containers")
(defsubcmd :path "docker" :name "pull" :description "Pull an image")
(defsubcmd :path "docker" :name "push" :description "Push an image")
(defsubcmd :path "docker" :name "rm" :description "Remove containers")
(defsubcmd :path "docker" :name "run" :description "Run a container")
(defsubcmd :path "docker" :name "stop" :description "Stop containers")
(defsubcmd :path "flux" :name "alert" :description "Alert rule")
(defsubcmd :path "flux" :name "alerts" :description "Alert rule")
(defsubcmd :path "flux" :name "bootstrap" :description "Bootstrap Flux on a cluster")
(defsubcmd :path "flux" :name "bucket" :description "S3-compatible source")
(defsubcmd :path "flux" :name "buckets" :description "S3-compatible source")
(defsubcmd :path "flux" :name "build" :description "Build kustomization locally")
(defsubcmd :path "flux" :name "check" :description "Pre-flight checks")
(defsubcmd :path "flux" :name "completion" :description "Shell completion")
(defsubcmd :path "flux" :name "create" :description "Create Flux resources")
(defsubcmd :path "flux" :name "delete" :description "Delete Flux resources")
(defsubcmd :path "flux" :name "diff" :description "Diff live vs desired")
(defsubcmd :path "flux" :name "events" :description "Flux events")
(defsubcmd :path "flux" :name "export" :description "Export resources as YAML")
(defsubcmd :path "flux" :name "get" :description "Display Flux resources")
(defsubcmd :path "flux" :name "gitrepositories" :description "Git source")
(defsubcmd :path "flux" :name "gitrepository" :description "Git source")
(defsubcmd :path "flux" :name "helmchart" :description "Helm chart artifact")
(defsubcmd :path "flux" :name "helmcharts" :description "Helm chart artifact")
(defsubcmd :path "flux" :name "helmrelease" :description "Helm release reconciler")
(defsubcmd :path "flux" :name "helmreleases" :description "Helm release reconciler")
(defsubcmd :path "flux" :name "helmrepositories" :description "Helm chart source")
(defsubcmd :path "flux" :name "helmrepository" :description "Helm chart source")
(defsubcmd :path "flux" :name "hr" :description "Helm release reconciler")
(defsubcmd :path "flux" :name "imagepolicies" :description "Image update policy")
(defsubcmd :path "flux" :name "imagepolicy" :description "Image update policy")
(defsubcmd :path "flux" :name "imagerepositories" :description "Image scan config")
(defsubcmd :path "flux" :name "imagerepository" :description "Image scan config")
(defsubcmd :path "flux" :name "imageupdateautomation" :description "Image auto-update")
(defsubcmd :path "flux" :name "imageupdateautomations" :description "Image auto-update")
(defsubcmd :path "flux" :name "install" :description "Install Flux components")
(defsubcmd :path "flux" :name "ks" :description "Kustomize reconciler")
(defsubcmd :path "flux" :name "kustomization" :description "Kustomize reconciler")
(defsubcmd :path "flux" :name "kustomizations" :description "Kustomize reconciler")
(defsubcmd :path "flux" :name "logs" :description "Flux controller logs")
(defsubcmd :path "flux" :name "ocirepositories" :description "OCI artifact source")
(defsubcmd :path "flux" :name "ocirepository" :description "OCI artifact source")
(defsubcmd :path "flux" :name "provider" :description "Notification provider")
(defsubcmd :path "flux" :name "providers" :description "Notification provider")
(defsubcmd :path "flux" :name "pull" :description "Pull artifact from OCI")
(defsubcmd :path "flux" :name "push" :description "Push artifact to OCI")
(defsubcmd :path "flux" :name "receiver" :description "Webhook receiver")
(defsubcmd :path "flux" :name "receivers" :description "Webhook receiver")
(defsubcmd :path "flux" :name "reconcile" :description "Trigger reconciliation")
(defsubcmd :path "flux" :name "resume" :description "Resume reconciliation")
(defsubcmd :path "flux" :name "stats" :description "Reconciliation statistics")
(defsubcmd :path "flux" :name "suspend" :description "Suspend reconciliation")
(defsubcmd :path "flux" :name "tag" :description "Tag an OCI artifact")
(defsubcmd :path "flux" :name "trace" :description "Trace a Flux resource")
(defsubcmd :path "flux" :name "tree" :description "Resource dependency tree")
(defsubcmd :path "flux" :name "uninstall" :description "Uninstall Flux")
(defsubcmd :path "flux" :name "version" :description "Flux CLI version")
(defsubcmd :path "gcloud" :name "app" :description "App Engine")
(defsubcmd :path "gcloud" :name "artifacts" :description "Artifact Registry")
(defsubcmd :path "gcloud" :name "auth" :description "Authentication")
(defsubcmd :path "gcloud" :name "builds" :description "Cloud Build")
(defsubcmd :path "gcloud" :name "compute" :description "Virtual machines & disks")
(defsubcmd :path "gcloud" :name "config" :description "CLI configuration")
(defsubcmd :path "gcloud" :name "container" :description "Kubernetes Engine")
(defsubcmd :path "gcloud" :name "deploy" :description "Cloud Deploy")
(defsubcmd :path "gcloud" :name "dns" :description "Cloud DNS")
(defsubcmd :path "gcloud" :name "firewall-rules" :description "Firewall rules")
(defsubcmd :path "gcloud" :name "functions" :description "Cloud Functions")
(defsubcmd :path "gcloud" :name "iam" :description "Identity & access")
(defsubcmd :path "gcloud" :name "kms" :description "Key Management")
(defsubcmd :path "gcloud" :name "logging" :description "Cloud Logging")
(defsubcmd :path "gcloud" :name "monitoring" :description "Cloud Monitoring")
(defsubcmd :path "gcloud" :name "networks" :description "VPC networks")
(defsubcmd :path "gcloud" :name "projects" :description "Project management")
(defsubcmd :path "gcloud" :name "pubsub" :description "Pub/Sub messaging")
(defsubcmd :path "gcloud" :name "run" :description "Cloud Run")
(defsubcmd :path "gcloud" :name "secrets" :description "Secret Manager")
(defsubcmd :path "gcloud" :name "services" :description "Service management")
(defsubcmd :path "gcloud" :name "sql" :description "Cloud SQL")
(defsubcmd :path "gcloud" :name "storage" :description "Cloud Storage")
(defsubcmd :path "git" :name "add" :description "Stage file contents")
(defsubcmd :path "git" :name "bisect" :description "Binary search for bugs")
(defsubcmd :path "git" :name "blame" :description "Show line-by-line authorship")
(defsubcmd :path "git" :name "branch" :description "List, create, or delete branches")
(defsubcmd :path "git" :name "checkout" :description "Switch branches or restore")
(defsubcmd :path "git" :name "cherry-pick" :description "Apply specific commits")
(defsubcmd :path "git" :name "clone" :description "Clone a repository")
(defsubcmd :path "git" :name "commit" :description "Record changes")
(defsubcmd :path "git" :name "diff" :description "Show changes")
(defsubcmd :path "git" :name "fetch" :description "Download objects and refs")
(defsubcmd :path "git" :name "init" :description "Create empty repo")
(defsubcmd :path "git" :name "log" :description "Show commit logs")
(defsubcmd :path "git" :name "merge" :description "Join branches")
(defsubcmd :path "git" :name "pull" :description "Fetch and merge")
(defsubcmd :path "git" :name "push" :description "Update remote refs")
(defsubcmd :path "git" :name "rebase" :description "Reapply commits on top")
(defsubcmd :path "git" :name "remote" :description "Manage remotes")
(defsubcmd :path "git" :name "reset" :description "Reset HEAD")
(defsubcmd :path "git" :name "restore" :description "Restore working tree files")
(defsubcmd :path "git" :name "revert" :description "Revert commits")
(defsubcmd :path "git" :name "show" :description "Show objects")
(defsubcmd :path "git" :name "stash" :description "Stash working changes")
(defsubcmd :path "git" :name "status" :description "Show working tree status")
(defsubcmd :path "git" :name "switch" :description "Switch branches")
(defsubcmd :path "git" :name "tag" :description "Create, list, or verify tags")
(defsubcmd :path "git" :name "worktree" :description "Manage working trees")
(defsubcmd :path "helm" :name "all" :description "All chart info")
(defsubcmd :path "helm" :name "chart" :description "Chart metadata")
(defsubcmd :path "helm" :name "completion" :description "Shell completion")
(defsubcmd :path "helm" :name "crds" :description "Chart CRDs")
(defsubcmd :path "helm" :name "create" :description "Create a new chart")
(defsubcmd :path "helm" :name "dep" :description "Manage dependencies")
(defsubcmd :path "helm" :name "dependency" :description "Manage dependencies")
(defsubcmd :path "helm" :name "env" :description "Helm environment info")
(defsubcmd :path "helm" :name "get" :description "Get release details")
(defsubcmd :path "helm" :name "history" :description "Release history")
(defsubcmd :path "helm" :name "install" :description "Install a chart")
(defsubcmd :path "helm" :name "lint" :description "Lint a chart")
(defsubcmd :path "helm" :name "list" :description "List releases")
(defsubcmd :path "helm" :name "ls" :description "List releases")
(defsubcmd :path "helm" :name "package" :description "Package a chart")
(defsubcmd :path "helm" :name "plugin" :description "Manage plugins")
(defsubcmd :path "helm" :name "pull" :description "Download a chart")
(defsubcmd :path "helm" :name "push" :description "Push to a registry")
(defsubcmd :path "helm" :name "readme" :description "Chart README")
(defsubcmd :path "helm" :name "registry" :description "Registry operations")
(defsubcmd :path "helm" :name "repo" :description "Manage chart repos")
(defsubcmd :path "helm" :name "rollback" :description "Rollback to a revision")
(defsubcmd :path "helm" :name "search" :description "Search for charts")
(defsubcmd :path "helm" :name "show" :description "Show chart information")
(defsubcmd :path "helm" :name "status" :description "Release status")
(defsubcmd :path "helm" :name "template" :description "Render templates locally")
(defsubcmd :path "helm" :name "test" :description "Test a release")
(defsubcmd :path "helm" :name "uninstall" :description "Uninstall a release")
(defsubcmd :path "helm" :name "upgrade" :description "Upgrade a release")
(defsubcmd :path "helm" :name "values" :description "Chart default values")
(defsubcmd :path "helm" :name "verify" :description "Verify a signed chart")
(defsubcmd :path "helm" :name "version" :description "Client version")
(defsubcmd :path "k" :name "annotate" :description "Update annotations")
(defsubcmd :path "k" :name "api-resources" :description "List API resource types")
(defsubcmd :path "k" :name "api-versions" :description "List API versions")
(defsubcmd :path "k" :name "apply" :description "Apply configuration")
(defsubcmd :path "k" :name "attach" :description "Attach to a container")
(defsubcmd :path "k" :name "auth" :description "Inspect authorization")
(defsubcmd :path "k" :name "autoscale" :description "Auto-scale a resource")
(defsubcmd :path "k" :name "certificate" :description "Certificate operations")
(defsubcmd :path "k" :name "cj" :description "Scheduled jobs")
(defsubcmd :path "k" :name "cluster-info" :description "Cluster endpoint info")
(defsubcmd :path "k" :name "clusterrole" :description "Cluster-wide permissions")
(defsubcmd :path "k" :name "clusterrolebinding" :description "Cluster role binding")
(defsubcmd :path "k" :name "clusterrolebindings" :description "Cluster role binding")
(defsubcmd :path "k" :name "clusterroles" :description "Cluster-wide permissions")
(defsubcmd :path "k" :name "cm" :description "Configuration data")
(defsubcmd :path "k" :name "completion" :description "Shell completion")
(defsubcmd :path "k" :name "config" :description "Modify kubeconfig")
(defsubcmd :path "k" :name "configmap" :description "Configuration data")
(defsubcmd :path "k" :name "configmaps" :description "Configuration data")
(defsubcmd :path "k" :name "cordon" :description "Mark node unschedulable")
(defsubcmd :path "k" :name "cp" :description "Copy files to/from containers")
(defsubcmd :path "k" :name "crd" :description "Custom API types")
(defsubcmd :path "k" :name "crds" :description "Custom API types")
(defsubcmd :path "k" :name "create" :description "Create from file or stdin")
(defsubcmd :path "k" :name "cronjob" :description "Scheduled jobs")
(defsubcmd :path "k" :name "cronjobs" :description "Scheduled jobs")
(defsubcmd :path "k" :name "customresourcedefinitions" :description "Custom API types")
(defsubcmd :path "k" :name "daemonset" :description "Per-node workloads")
(defsubcmd :path "k" :name "daemonsets" :description "Per-node workloads")
(defsubcmd :path "k" :name "debug" :description "Debug workloads")
(defsubcmd :path "k" :name "delete" :description "Delete resources")
(defsubcmd :path "k" :name "deploy" :description "Managed replicas")
(defsubcmd :path "k" :name "deployment" :description "Managed replicas")
(defsubcmd :path "k" :name "deployments" :description "Managed replicas")
(defsubcmd :path "k" :name "describe" :description "Show resource details")
(defsubcmd :path "k" :name "diff" :description "Diff live vs applied")
(defsubcmd :path "k" :name "drain" :description "Drain a node")
(defsubcmd :path "k" :name "ds" :description "Per-node workloads")
(defsubcmd :path "k" :name "edit" :description "Edit a resource")
(defsubcmd :path "k" :name "endpoints" :description "Service endpoints")
(defsubcmd :path "k" :name "ep" :description "Service endpoints")
(defsubcmd :path "k" :name "ev" :description "Cluster events")
(defsubcmd :path "k" :name "event" :description "Cluster events")
(defsubcmd :path "k" :name "events" :description "Cluster events")
(defsubcmd :path "k" :name "exec" :description "Execute in a container")
(defsubcmd :path "k" :name "explain" :description "Documentation of resources")
(defsubcmd :path "k" :name "expose" :description "Expose as a service")
(defsubcmd :path "k" :name "get" :description "Display resources")
(defsubcmd :path "k" :name "horizontalpodautoscalers" :description "Auto-scaling rules")
(defsubcmd :path "k" :name "hpa" :description "Auto-scaling rules")
(defsubcmd :path "k" :name "ing" :description "External access rules")
(defsubcmd :path "k" :name "ingress" :description "External access rules")
(defsubcmd :path "k" :name "ingresses" :description "External access rules")
(defsubcmd :path "k" :name "job" :description "Run-to-completion tasks")
(defsubcmd :path "k" :name "jobs" :description "Run-to-completion tasks")
(defsubcmd :path "k" :name "kustomize" :description "Build kustomization target")
(defsubcmd :path "k" :name "label" :description "Update labels")
(defsubcmd :path "k" :name "limitrange" :description "Resource constraints")
(defsubcmd :path "k" :name "limitranges" :description "Resource constraints")
(defsubcmd :path "k" :name "limits" :description "Resource constraints")
(defsubcmd :path "k" :name "logs" :description "Print container logs")
(defsubcmd :path "k" :name "namespace" :description "Resource scopes")
(defsubcmd :path "k" :name "namespaces" :description "Resource scopes")
(defsubcmd :path "k" :name "netpol" :description "Network access rules")
(defsubcmd :path "k" :name "networkpolicies" :description "Network access rules")
(defsubcmd :path "k" :name "networkpolicy" :description "Network access rules")
(defsubcmd :path "k" :name "no" :description "Cluster machines")
(defsubcmd :path "k" :name "node" :description "Cluster machines")
(defsubcmd :path "k" :name "nodes" :description "Cluster machines")
(defsubcmd :path "k" :name "ns" :description "Resource scopes")
(defsubcmd :path "k" :name "patch" :description "Patch a resource")
(defsubcmd :path "k" :name "pdb" :description "Disruption limits")
(defsubcmd :path "k" :name "persistentvolumeclaims" :description "Storage claims")
(defsubcmd :path "k" :name "persistentvolumes" :description "Storage volumes")
(defsubcmd :path "k" :name "plugin" :description "Plugin utilities")
(defsubcmd :path "k" :name "po" :description "Pod workloads")
(defsubcmd :path "k" :name "pod" :description "Pod workloads")
(defsubcmd :path "k" :name "poddisruptionbudgets" :description "Disruption limits")
(defsubcmd :path "k" :name "pods" :description "Pod workloads")
(defsubcmd :path "k" :name "port-forward" :description "Forward ports to a pod")
(defsubcmd :path "k" :name "proxy" :description "API server proxy")
(defsubcmd :path "k" :name "pv" :description "Storage volumes")
(defsubcmd :path "k" :name "pvc" :description "Storage claims")
(defsubcmd :path "k" :name "quota" :description "Namespace quotas")
(defsubcmd :path "k" :name "replace" :description "Replace a resource")
(defsubcmd :path "k" :name "replicaset" :description "Pod replica sets")
(defsubcmd :path "k" :name "replicasets" :description "Pod replica sets")
(defsubcmd :path "k" :name "resourcequota" :description "Namespace quotas")
(defsubcmd :path "k" :name "resourcequotas" :description "Namespace quotas")
(defsubcmd :path "k" :name "role" :description "Namespaced permissions")
(defsubcmd :path "k" :name "rolebinding" :description "Bind role to subject")
(defsubcmd :path "k" :name "rolebindings" :description "Bind role to subject")
(defsubcmd :path "k" :name "roles" :description "Namespaced permissions")
(defsubcmd :path "k" :name "rollout" :description "Manage rollouts")
(defsubcmd :path "k" :name "rs" :description "Pod replica sets")
(defsubcmd :path "k" :name "run" :description "Run a pod")
(defsubcmd :path "k" :name "sa" :description "Identities for pods")
(defsubcmd :path "k" :name "sc" :description "Storage provisioners")
(defsubcmd :path "k" :name "scale" :description "Scale a resource")
(defsubcmd :path "k" :name "secret" :description "Sensitive data")
(defsubcmd :path "k" :name "secrets" :description "Sensitive data")
(defsubcmd :path "k" :name "service" :description "Network endpoints")
(defsubcmd :path "k" :name "serviceaccount" :description "Identities for pods")
(defsubcmd :path "k" :name "serviceaccounts" :description "Identities for pods")
(defsubcmd :path "k" :name "services" :description "Network endpoints")
(defsubcmd :path "k" :name "set" :description "Set resource fields")
(defsubcmd :path "k" :name "statefulset" :description "Stateful workloads")
(defsubcmd :path "k" :name "statefulsets" :description "Stateful workloads")
(defsubcmd :path "k" :name "storageclass" :description "Storage provisioners")
(defsubcmd :path "k" :name "storageclasses" :description "Storage provisioners")
(defsubcmd :path "k" :name "sts" :description "Stateful workloads")
(defsubcmd :path "k" :name "svc" :description "Network endpoints")
(defsubcmd :path "k" :name "taint" :description "Set node taints")
(defsubcmd :path "k" :name "top" :description "Resource usage (CPU/memory)")
(defsubcmd :path "k" :name "uncordon" :description "Mark node schedulable")
(defsubcmd :path "k" :name "version" :description "Client and server version")
(defsubcmd :path "k" :name "wait" :description "Wait for a condition")
(defsubcmd :path "kubecolor" :name "annotate" :description "Update annotations")
(defsubcmd :path "kubecolor" :name "api-resources" :description "List API resource types")
(defsubcmd :path "kubecolor" :name "api-versions" :description "List API versions")
(defsubcmd :path "kubecolor" :name "apply" :description "Apply configuration")
(defsubcmd :path "kubecolor" :name "attach" :description "Attach to a container")
(defsubcmd :path "kubecolor" :name "auth" :description "Inspect authorization")
(defsubcmd :path "kubecolor" :name "autoscale" :description "Auto-scale a resource")
(defsubcmd :path "kubecolor" :name "certificate" :description "Certificate operations")
(defsubcmd :path "kubecolor" :name "cj" :description "Scheduled jobs")
(defsubcmd :path "kubecolor" :name "cluster-info" :description "Cluster endpoint info")
(defsubcmd :path "kubecolor" :name "clusterrole" :description "Cluster-wide permissions")
(defsubcmd :path "kubecolor" :name "clusterrolebinding" :description "Cluster role binding")
(defsubcmd :path "kubecolor" :name "clusterrolebindings" :description "Cluster role binding")
(defsubcmd :path "kubecolor" :name "clusterroles" :description "Cluster-wide permissions")
(defsubcmd :path "kubecolor" :name "cm" :description "Configuration data")
(defsubcmd :path "kubecolor" :name "completion" :description "Shell completion")
(defsubcmd :path "kubecolor" :name "config" :description "Modify kubeconfig")
(defsubcmd :path "kubecolor" :name "configmap" :description "Configuration data")
(defsubcmd :path "kubecolor" :name "configmaps" :description "Configuration data")
(defsubcmd :path "kubecolor" :name "cordon" :description "Mark node unschedulable")
(defsubcmd :path "kubecolor" :name "cp" :description "Copy files to/from containers")
(defsubcmd :path "kubecolor" :name "crd" :description "Custom API types")
(defsubcmd :path "kubecolor" :name "crds" :description "Custom API types")
(defsubcmd :path "kubecolor" :name "create" :description "Create from file or stdin")
(defsubcmd :path "kubecolor" :name "cronjob" :description "Scheduled jobs")
(defsubcmd :path "kubecolor" :name "cronjobs" :description "Scheduled jobs")
(defsubcmd :path "kubecolor" :name "customresourcedefinitions" :description "Custom API types")
(defsubcmd :path "kubecolor" :name "daemonset" :description "Per-node workloads")
(defsubcmd :path "kubecolor" :name "daemonsets" :description "Per-node workloads")
(defsubcmd :path "kubecolor" :name "debug" :description "Debug workloads")
(defsubcmd :path "kubecolor" :name "delete" :description "Delete resources")
(defsubcmd :path "kubecolor" :name "deploy" :description "Managed replicas")
(defsubcmd :path "kubecolor" :name "deployment" :description "Managed replicas")
(defsubcmd :path "kubecolor" :name "deployments" :description "Managed replicas")
(defsubcmd :path "kubecolor" :name "describe" :description "Show resource details")
(defsubcmd :path "kubecolor" :name "diff" :description "Diff live vs applied")
(defsubcmd :path "kubecolor" :name "drain" :description "Drain a node")
(defsubcmd :path "kubecolor" :name "ds" :description "Per-node workloads")
(defsubcmd :path "kubecolor" :name "edit" :description "Edit a resource")
(defsubcmd :path "kubecolor" :name "endpoints" :description "Service endpoints")
(defsubcmd :path "kubecolor" :name "ep" :description "Service endpoints")
(defsubcmd :path "kubecolor" :name "ev" :description "Cluster events")
(defsubcmd :path "kubecolor" :name "event" :description "Cluster events")
(defsubcmd :path "kubecolor" :name "events" :description "Cluster events")
(defsubcmd :path "kubecolor" :name "exec" :description "Execute in a container")
(defsubcmd :path "kubecolor" :name "explain" :description "Documentation of resources")
(defsubcmd :path "kubecolor" :name "expose" :description "Expose as a service")
(defsubcmd :path "kubecolor" :name "get" :description "Display resources")
(defsubcmd :path "kubecolor" :name "horizontalpodautoscalers" :description "Auto-scaling rules")
(defsubcmd :path "kubecolor" :name "hpa" :description "Auto-scaling rules")
(defsubcmd :path "kubecolor" :name "ing" :description "External access rules")
(defsubcmd :path "kubecolor" :name "ingress" :description "External access rules")
(defsubcmd :path "kubecolor" :name "ingresses" :description "External access rules")
(defsubcmd :path "kubecolor" :name "job" :description "Run-to-completion tasks")
(defsubcmd :path "kubecolor" :name "jobs" :description "Run-to-completion tasks")
(defsubcmd :path "kubecolor" :name "kustomize" :description "Build kustomization target")
(defsubcmd :path "kubecolor" :name "label" :description "Update labels")
(defsubcmd :path "kubecolor" :name "limitrange" :description "Resource constraints")
(defsubcmd :path "kubecolor" :name "limitranges" :description "Resource constraints")
(defsubcmd :path "kubecolor" :name "limits" :description "Resource constraints")
(defsubcmd :path "kubecolor" :name "logs" :description "Print container logs")
(defsubcmd :path "kubecolor" :name "namespace" :description "Resource scopes")
(defsubcmd :path "kubecolor" :name "namespaces" :description "Resource scopes")
(defsubcmd :path "kubecolor" :name "netpol" :description "Network access rules")
(defsubcmd :path "kubecolor" :name "networkpolicies" :description "Network access rules")
(defsubcmd :path "kubecolor" :name "networkpolicy" :description "Network access rules")
(defsubcmd :path "kubecolor" :name "no" :description "Cluster machines")
(defsubcmd :path "kubecolor" :name "node" :description "Cluster machines")
(defsubcmd :path "kubecolor" :name "nodes" :description "Cluster machines")
(defsubcmd :path "kubecolor" :name "ns" :description "Resource scopes")
(defsubcmd :path "kubecolor" :name "patch" :description "Patch a resource")
(defsubcmd :path "kubecolor" :name "pdb" :description "Disruption limits")
(defsubcmd :path "kubecolor" :name "persistentvolumeclaims" :description "Storage claims")
(defsubcmd :path "kubecolor" :name "persistentvolumes" :description "Storage volumes")
(defsubcmd :path "kubecolor" :name "plugin" :description "Plugin utilities")
(defsubcmd :path "kubecolor" :name "po" :description "Pod workloads")
(defsubcmd :path "kubecolor" :name "pod" :description "Pod workloads")
(defsubcmd :path "kubecolor" :name "poddisruptionbudgets" :description "Disruption limits")
(defsubcmd :path "kubecolor" :name "pods" :description "Pod workloads")
(defsubcmd :path "kubecolor" :name "port-forward" :description "Forward ports to a pod")
(defsubcmd :path "kubecolor" :name "proxy" :description "API server proxy")
(defsubcmd :path "kubecolor" :name "pv" :description "Storage volumes")
(defsubcmd :path "kubecolor" :name "pvc" :description "Storage claims")
(defsubcmd :path "kubecolor" :name "quota" :description "Namespace quotas")
(defsubcmd :path "kubecolor" :name "replace" :description "Replace a resource")
(defsubcmd :path "kubecolor" :name "replicaset" :description "Pod replica sets")
(defsubcmd :path "kubecolor" :name "replicasets" :description "Pod replica sets")
(defsubcmd :path "kubecolor" :name "resourcequota" :description "Namespace quotas")
(defsubcmd :path "kubecolor" :name "resourcequotas" :description "Namespace quotas")
(defsubcmd :path "kubecolor" :name "role" :description "Namespaced permissions")
(defsubcmd :path "kubecolor" :name "rolebinding" :description "Bind role to subject")
(defsubcmd :path "kubecolor" :name "rolebindings" :description "Bind role to subject")
(defsubcmd :path "kubecolor" :name "roles" :description "Namespaced permissions")
(defsubcmd :path "kubecolor" :name "rollout" :description "Manage rollouts")
(defsubcmd :path "kubecolor" :name "rs" :description "Pod replica sets")
(defsubcmd :path "kubecolor" :name "run" :description "Run a pod")
(defsubcmd :path "kubecolor" :name "sa" :description "Identities for pods")
(defsubcmd :path "kubecolor" :name "sc" :description "Storage provisioners")
(defsubcmd :path "kubecolor" :name "scale" :description "Scale a resource")
(defsubcmd :path "kubecolor" :name "secret" :description "Sensitive data")
(defsubcmd :path "kubecolor" :name "secrets" :description "Sensitive data")
(defsubcmd :path "kubecolor" :name "service" :description "Network endpoints")
(defsubcmd :path "kubecolor" :name "serviceaccount" :description "Identities for pods")
(defsubcmd :path "kubecolor" :name "serviceaccounts" :description "Identities for pods")
(defsubcmd :path "kubecolor" :name "services" :description "Network endpoints")
(defsubcmd :path "kubecolor" :name "set" :description "Set resource fields")
(defsubcmd :path "kubecolor" :name "statefulset" :description "Stateful workloads")
(defsubcmd :path "kubecolor" :name "statefulsets" :description "Stateful workloads")
(defsubcmd :path "kubecolor" :name "storageclass" :description "Storage provisioners")
(defsubcmd :path "kubecolor" :name "storageclasses" :description "Storage provisioners")
(defsubcmd :path "kubecolor" :name "sts" :description "Stateful workloads")
(defsubcmd :path "kubecolor" :name "svc" :description "Network endpoints")
(defsubcmd :path "kubecolor" :name "taint" :description "Set node taints")
(defsubcmd :path "kubecolor" :name "top" :description "Resource usage (CPU/memory)")
(defsubcmd :path "kubecolor" :name "uncordon" :description "Mark node schedulable")
(defsubcmd :path "kubecolor" :name "version" :description "Client and server version")
(defsubcmd :path "kubecolor" :name "wait" :description "Wait for a condition")
(defsubcmd :path "kubectl" :name "annotate" :description "Update annotations")
(defsubcmd :path "kubectl" :name "api-resources" :description "List API resource types")
(defsubcmd :path "kubectl" :name "api-versions" :description "List API versions")
(defsubcmd :path "kubectl" :name "apply" :description "Apply configuration")
(defsubcmd :path "kubectl" :name "attach" :description "Attach to a container")
(defsubcmd :path "kubectl" :name "auth" :description "Inspect authorization")
(defsubcmd :path "kubectl" :name "autoscale" :description "Auto-scale a resource")
(defsubcmd :path "kubectl" :name "certificate" :description "Certificate operations")
(defsubcmd :path "kubectl" :name "cj" :description "Scheduled jobs")
(defsubcmd :path "kubectl" :name "cluster-info" :description "Cluster endpoint info")
(defsubcmd :path "kubectl" :name "clusterrole" :description "Cluster-wide permissions")
(defsubcmd :path "kubectl" :name "clusterrolebinding" :description "Cluster role binding")
(defsubcmd :path "kubectl" :name "clusterrolebindings" :description "Cluster role binding")
(defsubcmd :path "kubectl" :name "clusterroles" :description "Cluster-wide permissions")
(defsubcmd :path "kubectl" :name "cm" :description "Configuration data")
(defsubcmd :path "kubectl" :name "completion" :description "Shell completion")
(defsubcmd :path "kubectl" :name "config" :description "Modify kubeconfig")
(defsubcmd :path "kubectl" :name "configmap" :description "Configuration data")
(defsubcmd :path "kubectl" :name "configmaps" :description "Configuration data")
(defsubcmd :path "kubectl" :name "cordon" :description "Mark node unschedulable")
(defsubcmd :path "kubectl" :name "cp" :description "Copy files to/from containers")
(defsubcmd :path "kubectl" :name "crd" :description "Custom API types")
(defsubcmd :path "kubectl" :name "crds" :description "Custom API types")
(defsubcmd :path "kubectl" :name "create" :description "Create from file or stdin")
(defsubcmd :path "kubectl" :name "cronjob" :description "Scheduled jobs")
(defsubcmd :path "kubectl" :name "cronjobs" :description "Scheduled jobs")
(defsubcmd :path "kubectl" :name "customresourcedefinitions" :description "Custom API types")
(defsubcmd :path "kubectl" :name "daemonset" :description "Per-node workloads")
(defsubcmd :path "kubectl" :name "daemonsets" :description "Per-node workloads")
(defsubcmd :path "kubectl" :name "debug" :description "Debug workloads")
(defsubcmd :path "kubectl" :name "delete" :description "Delete resources")
(defsubcmd :path "kubectl" :name "deploy" :description "Managed replicas")
(defsubcmd :path "kubectl" :name "deployment" :description "Managed replicas")
(defsubcmd :path "kubectl" :name "deployments" :description "Managed replicas")
(defsubcmd :path "kubectl" :name "describe" :description "Show resource details")
(defsubcmd :path "kubectl" :name "diff" :description "Diff live vs applied")
(defsubcmd :path "kubectl" :name "drain" :description "Drain a node")
(defsubcmd :path "kubectl" :name "ds" :description "Per-node workloads")
(defsubcmd :path "kubectl" :name "edit" :description "Edit a resource")
(defsubcmd :path "kubectl" :name "endpoints" :description "Service endpoints")
(defsubcmd :path "kubectl" :name "ep" :description "Service endpoints")
(defsubcmd :path "kubectl" :name "ev" :description "Cluster events")
(defsubcmd :path "kubectl" :name "event" :description "Cluster events")
(defsubcmd :path "kubectl" :name "events" :description "Cluster events")
(defsubcmd :path "kubectl" :name "exec" :description "Execute in a container")
(defsubcmd :path "kubectl" :name "explain" :description "Documentation of resources")
(defsubcmd :path "kubectl" :name "expose" :description "Expose as a service")
(defsubcmd :path "kubectl" :name "get" :description "Display resources")
(defsubcmd :path "kubectl" :name "horizontalpodautoscalers" :description "Auto-scaling rules")
(defsubcmd :path "kubectl" :name "hpa" :description "Auto-scaling rules")
(defsubcmd :path "kubectl" :name "ing" :description "External access rules")
(defsubcmd :path "kubectl" :name "ingress" :description "External access rules")
(defsubcmd :path "kubectl" :name "ingresses" :description "External access rules")
(defsubcmd :path "kubectl" :name "job" :description "Run-to-completion tasks")
(defsubcmd :path "kubectl" :name "jobs" :description "Run-to-completion tasks")
(defsubcmd :path "kubectl" :name "kustomize" :description "Build kustomization target")
(defsubcmd :path "kubectl" :name "label" :description "Update labels")
(defsubcmd :path "kubectl" :name "limitrange" :description "Resource constraints")
(defsubcmd :path "kubectl" :name "limitranges" :description "Resource constraints")
(defsubcmd :path "kubectl" :name "limits" :description "Resource constraints")
(defsubcmd :path "kubectl" :name "logs" :description "Print container logs")
(defsubcmd :path "kubectl" :name "namespace" :description "Resource scopes")
(defsubcmd :path "kubectl" :name "namespaces" :description "Resource scopes")
(defsubcmd :path "kubectl" :name "netpol" :description "Network access rules")
(defsubcmd :path "kubectl" :name "networkpolicies" :description "Network access rules")
(defsubcmd :path "kubectl" :name "networkpolicy" :description "Network access rules")
(defsubcmd :path "kubectl" :name "no" :description "Cluster machines")
(defsubcmd :path "kubectl" :name "node" :description "Cluster machines")
(defsubcmd :path "kubectl" :name "nodes" :description "Cluster machines")
(defsubcmd :path "kubectl" :name "ns" :description "Resource scopes")
(defsubcmd :path "kubectl" :name "patch" :description "Patch a resource")
(defsubcmd :path "kubectl" :name "pdb" :description "Disruption limits")
(defsubcmd :path "kubectl" :name "persistentvolumeclaims" :description "Storage claims")
(defsubcmd :path "kubectl" :name "persistentvolumes" :description "Storage volumes")
(defsubcmd :path "kubectl" :name "plugin" :description "Plugin utilities")
(defsubcmd :path "kubectl" :name "po" :description "Pod workloads")
(defsubcmd :path "kubectl" :name "pod" :description "Pod workloads")
(defsubcmd :path "kubectl" :name "poddisruptionbudgets" :description "Disruption limits")
(defsubcmd :path "kubectl" :name "pods" :description "Pod workloads")
(defsubcmd :path "kubectl" :name "port-forward" :description "Forward ports to a pod")
(defsubcmd :path "kubectl" :name "proxy" :description "API server proxy")
(defsubcmd :path "kubectl" :name "pv" :description "Storage volumes")
(defsubcmd :path "kubectl" :name "pvc" :description "Storage claims")
(defsubcmd :path "kubectl" :name "quota" :description "Namespace quotas")
(defsubcmd :path "kubectl" :name "replace" :description "Replace a resource")
(defsubcmd :path "kubectl" :name "replicaset" :description "Pod replica sets")
(defsubcmd :path "kubectl" :name "replicasets" :description "Pod replica sets")
(defsubcmd :path "kubectl" :name "resourcequota" :description "Namespace quotas")
(defsubcmd :path "kubectl" :name "resourcequotas" :description "Namespace quotas")
(defsubcmd :path "kubectl" :name "role" :description "Namespaced permissions")
(defsubcmd :path "kubectl" :name "rolebinding" :description "Bind role to subject")
(defsubcmd :path "kubectl" :name "rolebindings" :description "Bind role to subject")
(defsubcmd :path "kubectl" :name "roles" :description "Namespaced permissions")
(defsubcmd :path "kubectl" :name "rollout" :description "Manage rollouts")
(defsubcmd :path "kubectl" :name "rs" :description "Pod replica sets")
(defsubcmd :path "kubectl" :name "run" :description "Run a pod")
(defsubcmd :path "kubectl" :name "sa" :description "Identities for pods")
(defsubcmd :path "kubectl" :name "sc" :description "Storage provisioners")
(defsubcmd :path "kubectl" :name "scale" :description "Scale a resource")
(defsubcmd :path "kubectl" :name "secret" :description "Sensitive data")
(defsubcmd :path "kubectl" :name "secrets" :description "Sensitive data")
(defsubcmd :path "kubectl" :name "service" :description "Network endpoints")
(defsubcmd :path "kubectl" :name "serviceaccount" :description "Identities for pods")
(defsubcmd :path "kubectl" :name "serviceaccounts" :description "Identities for pods")
(defsubcmd :path "kubectl" :name "services" :description "Network endpoints")
(defsubcmd :path "kubectl" :name "set" :description "Set resource fields")
(defsubcmd :path "kubectl" :name "statefulset" :description "Stateful workloads")
(defsubcmd :path "kubectl" :name "statefulsets" :description "Stateful workloads")
(defsubcmd :path "kubectl" :name "storageclass" :description "Storage provisioners")
(defsubcmd :path "kubectl" :name "storageclasses" :description "Storage provisioners")
(defsubcmd :path "kubectl" :name "sts" :description "Stateful workloads")
(defsubcmd :path "kubectl" :name "svc" :description "Network endpoints")
(defsubcmd :path "kubectl" :name "taint" :description "Set node taints")
(defsubcmd :path "kubectl" :name "top" :description "Resource usage (CPU/memory)")
(defsubcmd :path "kubectl" :name "uncordon" :description "Mark node schedulable")
(defsubcmd :path "kubectl" :name "version" :description "Client and server version")
(defsubcmd :path "kubectl" :name "wait" :description "Wait for a condition")
(defsubcmd :path "nix" :name "build" :description "Build a derivation")
(defsubcmd :path "nix" :name "develop" :description "Enter dev shell")
(defsubcmd :path "nix" :name "eval" :description "Evaluate expression")
(defsubcmd :path "nix" :name "flake" :description "Flake operations")
(defsubcmd :path "nix" :name "path-info" :description "Store path info")
(defsubcmd :path "nix" :name "profile" :description "Manage profiles")
(defsubcmd :path "nix" :name "repl" :description "Interactive REPL")
(defsubcmd :path "nix" :name "run" :description "Run a flake app")
(defsubcmd :path "nix" :name "search" :description "Search packages")
(defsubcmd :path "nix" :name "store" :description "Nix store operations")
(defsubcmd :path "nix.flake" :name "check" :description "Check flake outputs")
(defsubcmd :path "nix.flake" :name "lock" :description "Update flake.lock")
(defsubcmd :path "nix.flake" :name "metadata" :description "Show flake metadata")
(defsubcmd :path "nix.flake" :name "show" :description "Show flake outputs")
(defsubcmd :path "nix.flake" :name "update" :description "Update flake inputs")
(defsubcmd :path "npm" :name "audit" :description "Run security audit")
(defsubcmd :path "npm" :name "build" :description "Build the project")
(defsubcmd :path "npm" :name "init" :description "Create package.json")
(defsubcmd :path "npm" :name "install" :description "Install dependencies")
(defsubcmd :path "npm" :name "link" :description "Symlink a package")
(defsubcmd :path "npm" :name "outdated" :description "Check for outdated packages")
(defsubcmd :path "npm" :name "publish" :description "Publish to registry")
(defsubcmd :path "npm" :name "run" :description "Run a script")
(defsubcmd :path "npm" :name "start" :description "Start the application")
(defsubcmd :path "npm" :name "test" :description "Run tests")
(defsubcmd :path "npm" :name "uninstall" :description "Remove a package")
(defsubcmd :path "npm" :name "update" :description "Update packages")
(defsubcmd :path "pnpm" :name "audit" :description "Run security audit")
(defsubcmd :path "pnpm" :name "build" :description "Build the project")
(defsubcmd :path "pnpm" :name "init" :description "Create package.json")
(defsubcmd :path "pnpm" :name "install" :description "Install dependencies")
(defsubcmd :path "pnpm" :name "link" :description "Symlink a package")
(defsubcmd :path "pnpm" :name "outdated" :description "Check for outdated packages")
(defsubcmd :path "pnpm" :name "publish" :description "Publish to registry")
(defsubcmd :path "pnpm" :name "run" :description "Run a script")
(defsubcmd :path "pnpm" :name "start" :description "Start the application")
(defsubcmd :path "pnpm" :name "test" :description "Run tests")
(defsubcmd :path "pnpm" :name "uninstall" :description "Remove a package")
(defsubcmd :path "pnpm" :name "update" :description "Update packages")
(defsubcmd :path "podman" :name "build" :description "Build an image")
(defsubcmd :path "podman" :name "compose" :description "Docker Compose")
(defsubcmd :path "podman" :name "exec" :description "Execute in container")
(defsubcmd :path "podman" :name "images" :description "List images")
(defsubcmd :path "podman" :name "logs" :description "Container logs")
(defsubcmd :path "podman" :name "ps" :description "List containers")
(defsubcmd :path "podman" :name "pull" :description "Pull an image")
(defsubcmd :path "podman" :name "push" :description "Push an image")
(defsubcmd :path "podman" :name "rm" :description "Remove containers")
(defsubcmd :path "podman" :name "run" :description "Run a container")
(defsubcmd :path "podman" :name "stop" :description "Stop containers")
(defsubcmd :path "terraform" :name "apply" :description "Apply changes")
(defsubcmd :path "terraform" :name "destroy" :description "Destroy infrastructure")
(defsubcmd :path "terraform" :name "fmt" :description "Format configuration")
(defsubcmd :path "terraform" :name "import" :description "Import existing resources")
(defsubcmd :path "terraform" :name "init" :description "Initialize working directory")
(defsubcmd :path "terraform" :name "output" :description "Show output values")
(defsubcmd :path "terraform" :name "plan" :description "Show execution plan")
(defsubcmd :path "terraform" :name "providers" :description "Show providers")
(defsubcmd :path "terraform" :name "refresh" :description "Update state")
(defsubcmd :path "terraform" :name "state" :description "Manage state")
(defsubcmd :path "terraform" :name "validate" :description "Validate configuration")
(defsubcmd :path "terraform" :name "workspace" :description "Manage workspaces")
(defsubcmd :path "tofu" :name "apply" :description "Apply changes")
(defsubcmd :path "tofu" :name "destroy" :description "Destroy infrastructure")
(defsubcmd :path "tofu" :name "fmt" :description "Format configuration")
(defsubcmd :path "tofu" :name "import" :description "Import existing resources")
(defsubcmd :path "tofu" :name "init" :description "Initialize working directory")
(defsubcmd :path "tofu" :name "output" :description "Show output values")
(defsubcmd :path "tofu" :name "plan" :description "Show execution plan")
(defsubcmd :path "tofu" :name "providers" :description "Show providers")
(defsubcmd :path "tofu" :name "refresh" :description "Update state")
(defsubcmd :path "tofu" :name "state" :description "Manage state")
(defsubcmd :path "tofu" :name "validate" :description "Validate configuration")
(defsubcmd :path "tofu" :name "workspace" :description "Manage workspaces")
(defsubcmd :path "yarn" :name "audit" :description "Run security audit")
(defsubcmd :path "yarn" :name "build" :description "Build the project")
(defsubcmd :path "yarn" :name "init" :description "Create package.json")
(defsubcmd :path "yarn" :name "install" :description "Install dependencies")
(defsubcmd :path "yarn" :name "link" :description "Symlink a package")
(defsubcmd :path "yarn" :name "outdated" :description "Check for outdated packages")
(defsubcmd :path "yarn" :name "publish" :description "Publish to registry")
(defsubcmd :path "yarn" :name "run" :description "Run a script")
(defsubcmd :path "yarn" :name "start" :description "Start the application")
(defsubcmd :path "yarn" :name "test" :description "Run tests")
(defsubcmd :path "yarn" :name "uninstall" :description "Remove a package")
(defsubcmd :path "yarn" :name "update" :description "Update packages")

