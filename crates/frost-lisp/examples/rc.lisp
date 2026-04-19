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
