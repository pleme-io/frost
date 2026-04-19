;; Example frost rc — declarative shell config via tatara-lisp.
;;
;; Loaded at frost startup from $FROSTRC (or ~/.frostrc.lisp by default).
;; Any subset of these forms is valid; order within the file does not
;; matter — the applicator does one pass per domain.

;; ── Aliases ─────────────────────────────────────────────────────────
(defalias :name "ll"  :value "ls -la")
(defalias :name "la"  :value "ls -A")
(defalias :name "gst" :value "git status -sb")
(defalias :name "..." :value "cd ../..")

;; ── Shell options ──────────────────────────────────────────────────
(defopts :enable ("extendedglob" "globdots" "promptsubst" "histignoredups")
         :disable ("beep"))

;; ── Environment ────────────────────────────────────────────────────
(defenv :name "EDITOR" :value "blnvim"      :export #t)
(defenv :name "PAGER"  :value "less -R"     :export #t)
(defenv :name "LANG"   :value "en_US.UTF-8" :export #t)

;; Not-exported variables (visible to shell but not inherited by subprocs).
(defenv :name "FROST_GREETING" :value "welcome to frost")

;; ── Prompt ─────────────────────────────────────────────────────────
;; ANSI colors come from frost-prompt's %F{…}/%K{…}/%B/%U escapes.
;; Setting :prompt-subst #t lets $VAR inside the template expand too.
(defprompt :ps1 "%F{green}%n%f@%F{blue}%m%f %~ %# "
           :ps2 "> "
           :prompt-subst #t)

;; ── Lifecycle hooks ────────────────────────────────────────────────
;; Body is parsed as shell source at load time and stored under a
;; private function name; the REPL invokes it at the right point.
(defhook :event "precmd"
         :body "echo")                               ; blank line before prompt

(defhook :event "preexec"
         :body "echo 'running: ' $1")                ; announce each command

;; ── Signal traps ───────────────────────────────────────────────────
(deftrap :signal "INT"  :body "echo interrupted")
(deftrap :signal "EXIT" :body "echo goodbye")

;; ── Keybindings ────────────────────────────────────────────────────
;; Stored under __frost_bind_<canonical-key> in env.functions. ZLE
;; dispatcher wire-up lands in a follow-up; authoring works today.
(defbind :key "C-x e" :action "edit-line-in-editor")
(defbind :key "M-?"   :action "help")

;; ── Per-command completions ────────────────────────────────────────
;; Stored as a JSON payload in __frost_complete_<command>; the
;; FrostCompleter will consult these for argument-position suggestions.
(defcompletion :command "git"
               :args ("status" "diff" "log" "commit" "push" "pull")
               :description "version control")

(defcompletion :command "kubectl"
               :args ("get" "describe" "apply" "delete" "logs" "exec"))

;; ── Shell functions (Lisp-declared, shell-bodied) ──────────────────
(defun :name "mkcd"
       :body "mkdir -p \"$1\" && cd \"$1\"")

(defun :name "up"
       :body "cd $(printf '../%.0s' $(seq 1 ${1:-1}))")
