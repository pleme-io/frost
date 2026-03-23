use compact_str::CompactString;
use frost_lexer::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub commands: Vec<CompleteCommand>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompleteCommand {
    pub list: List,
    pub is_async: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct List {
    pub first: Pipeline,
    pub rest: Vec<(ListOp, Pipeline)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub bang: bool,
    pub commands: Vec<Command>,
    pub pipe_stderr: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Simple(SimpleCommand),
    Subshell(Subshell),
    BraceGroup(BraceGroup),
    If(Box<IfClause>),
    For(Box<ForClause>),
    While(Box<WhileClause>),
    Until(Box<UntilClause>),
    Case(Box<CaseClause>),
    Select(Box<SelectClause>),
    FunctionDef(Box<FunctionDef>),
    Coproc(Box<Coproc>),
    Time(Box<TimeClause>),
    /// `(( expr ))` — arithmetic evaluation command.
    ArithCmd(CompactString),
    /// `[[ expr ]]` — conditional expression.
    Cond(Box<CondExpr>),
    /// `for (( init; cond; step )) { body }` — C-style for loop.
    CFor(Box<CForClause>),
    /// `repeat N { body }` — repeat loop.
    Repeat(Box<RepeatClause>),
    /// `{ try } always { always }` — try-always block.
    TryAlways(Box<TryAlwaysClause>),
}

/// Conditional expression inside `[[ … ]]`.
#[derive(Debug, Clone, PartialEq)]
pub enum CondExpr {
    Unary(CondOp, Word),
    Binary(Word, CondOp, Word),
    Not(Box<CondExpr>),
    And(Box<CondExpr>, Box<CondExpr>),
    Or(Box<CondExpr>, Box<CondExpr>),
}

/// Operators for `[[ … ]]` conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CondOp {
    // File tests
    FileExists,         // -e, -a
    IsFile,             // -f
    IsDir,              // -d
    IsSymlink,          // -L, -h
    IsReadable,         // -r
    IsWritable,         // -w
    IsExecutable,       // -x
    IsNonEmpty,         // -s
    IsBlockDev,         // -b
    IsCharDev,          // -c
    IsFifo,             // -p
    IsSocket,           // -S
    IsSetuid,           // -u
    IsSetgid,           // -g
    IsSticky,           // -k
    OwnedByUser,        // -O
    OwnedByGroup,       // -G
    ModifiedSinceRead,  // -N
    IsTty,              // -t
    OptionSet,          // -o
    VarIsSet,           // -v
    // String tests
    StrEmpty,           // -z
    StrNonEmpty,        // -n
    StrEq,              // == or =
    StrNeq,             // !=
    StrLt,              // <
    StrGt,              // >
    StrMatch,           // =~
    // Integer tests
    IntEq, IntNe, IntLt, IntLe, IntGt, IntGe,
    // File comparisons
    NewerThan,          // -nt
    OlderThan,          // -ot
    SameFile,           // -ef
}

/// C-style for loop: `for (( init; cond; step )) { body }`.
#[derive(Debug, Clone, PartialEq)]
pub struct CForClause {
    pub init: CompactString,
    pub condition: CompactString,
    pub step: CompactString,
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

/// `repeat N { body }` loop.
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatClause {
    pub count: Word,
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

/// `{ try } always { always }` block.
#[derive(Debug, Clone, PartialEq)]
pub struct TryAlwaysClause {
    pub try_body: Vec<CompleteCommand>,
    pub always_body: Vec<CompleteCommand>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    pub assignments: Vec<Assignment>,
    pub words: Vec<Word>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub parts: Vec<WordPart>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WordPart {
    Literal(CompactString),
    SingleQuoted(CompactString),
    DoubleQuoted(Vec<WordPart>),
    DollarVar(CompactString),
    DollarBrace {
        param: CompactString,
        operator: Option<CompactString>,
        arg: Option<Box<Word>>,
    },
    /// Structured parameter expansion — rich `${...}` forms.
    ParamExp(Box<ParamExpansion>),
    CommandSub(Box<Program>),
    ArithSub(CompactString),
    Glob(GlobKind),
    Tilde(CompactString),
    /// Brace expansion: `{a,b,c}` or `{1..10}`.
    BraceExp(BraceExpansion),
    /// Process substitution: `<(cmd)` or `>(cmd)`.
    ProcessSub {
        kind: ProcessSubKind,
        body: Box<Program>,
    },
    /// Extended glob: `*(pat)`, `+(pat)`, `?(pat)`, `@(pat)`, `!(pat)`.
    ExtGlob {
        op: ExtGlobOp,
        pattern: CompactString,
    },
}

// ── Structured parameter expansion ────────────────────────────────

/// Rich parameter expansion: `${(flags)#name[subscript]modifier}`.
///
/// Follows the 14-field model from mvdan/sh:
/// flags → length → nested → name → subscript → modifier.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamExpansion {
    /// `${(flags)name}` — processing flags like `(L)`, `(U)`, `(f)`, etc.
    pub flags: Vec<ParamFlag>,
    /// `${#name}` — length operator.
    pub length: bool,
    /// `${+name}` — is-set test.
    pub is_set_test: bool,
    /// The parameter name (or special: `?`, `$`, `#`, `*`, `@`, digits).
    pub name: CompactString,
    /// `${${inner}}` — nested expansion (indirect).
    pub nested: Option<Box<ParamExpansion>>,
    /// `${name[subscript]}` — array subscript.
    pub subscript: Option<Subscript>,
    /// The operation to apply: default, trim, replace, case, substring, etc.
    pub modifier: Option<ParamModifier>,
}

/// Array subscript expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum Subscript {
    /// Numeric index (possibly negative): `${arr[3]}`, `${arr[-1]}`.
    Index(CompactString),
    /// `${arr[@]}` — all elements as separate words.
    All,
    /// `${arr[*]}` — all elements joined.
    Star,
    /// Pattern search: `${arr[(r)pat]}` or `${arr[(i)idx]}`.
    Pattern {
        reverse: bool,
        pattern: CompactString,
    },
}

/// Modifier operations on parameter values.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamModifier {
    /// `${name:-word}` or `${name-word}` — use default.
    Default {
        colon: bool,
        word: Box<Word>,
    },
    /// `${name:=word}` or `${name=word}` — assign default.
    Assign {
        colon: bool,
        word: Box<Word>,
    },
    /// `${name:+word}` or `${name+word}` — use alternative.
    Alternative {
        colon: bool,
        word: Box<Word>,
    },
    /// `${name:?word}` or `${name?word}` — error if unset.
    Error {
        colon: bool,
        word: Box<Word>,
    },
    /// `${name#pat}` or `${name##pat}` — remove prefix.
    TrimPrefix {
        longest: bool,
        pattern: Box<Word>,
    },
    /// `${name%pat}` or `${name%%pat}` — remove suffix.
    TrimSuffix {
        longest: bool,
        pattern: Box<Word>,
    },
    /// `${name/pat/rep}` or `${name//pat/rep}` — substitution.
    Substitute {
        anchor: SubAnchor,
        pattern: Box<Word>,
        replacement: Option<Box<Word>>,
    },
    /// `${name:offset}` or `${name:offset:length}` — substring.
    Substring {
        offset: Box<Word>,
        length: Option<Box<Word>>,
    },
    /// `${name^pat}`, `${name^^pat}`, `${name,pat}`, `${name,,pat}`.
    Case(CaseOp),
}

/// Anchor mode for `${name/pat/rep}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAnchor {
    /// `${name/pat/rep}` — first match.
    First,
    /// `${name//pat/rep}` — all matches.
    All,
    /// `${name/#pat/rep}` — anchored at start.
    Start,
    /// `${name/%pat/rep}` — anchored at end.
    End,
}

/// Case transformation in `${name^}`, `${name^^}`, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseOp {
    /// `${name^}` — uppercase first char.
    UpperFirst,
    /// `${name^^}` — uppercase all.
    UpperAll,
    /// `${name,}` — lowercase first char.
    LowerFirst,
    /// `${name,,}` — lowercase all.
    LowerAll,
}

/// Flags in `${(flags)name}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamFlag {
    Lower,          // (L)
    Upper,          // (U)
    Capitalize,     // (C)
    Split,          // (f) — split on newlines
    Join,           // (F) — join with newlines
    SplitSep(char), // (s:sep:)
    JoinSep(char),  // (j:sep:)
    SortAsc,        // (o)
    SortDesc,       // (O)
    Unique,         // (u)
    Keys,           // (k)
    Values,         // (v)
    TypeFlag,       // (t) — type of variable
    Prompt,         // (P) — prompt expansion
    Quote,          // (q)
    Unquote,        // (Q)
    Expand,         // (e) — perform expansion
    Words,          // (z) — split into words
    Visible,        // (V) — make special chars visible
}

// ── Brace expansion ────────────────────────────────────────────────

/// Brace expansion: `{a,b,c}` or `{1..10..2}`.
#[derive(Debug, Clone, PartialEq)]
pub enum BraceExpansion {
    /// `{a,b,c}` — list of alternatives.
    List(Vec<CompactString>),
    /// `{start..end[..step]}` — range (numeric or char).
    Range {
        start: CompactString,
        end: CompactString,
        step: Option<CompactString>,
    },
}

/// Process substitution direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSubKind {
    /// `<(cmd)` — read from process.
    Input,
    /// `>(cmd)` — write to process.
    Output,
}

/// Extended glob operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtGlobOp {
    /// `*(pat)` — zero or more.
    Star,
    /// `+(pat)` — one or more.
    Plus,
    /// `?(pat)` — zero or one.
    Question,
    /// `@(pat)` — exactly one.
    At,
    /// `!(pat)` — anything except.
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobKind {
    Star,
    Question,
    At,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    pub fd: Option<u32>,
    pub op: RedirectOp,
    pub target: Word,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectOp {
    Less,
    Greater,
    DoubleGreater,
    GreaterPipe,
    GreaterBang,
    AmpGreater,
    AmpDoubleGreater,
    DoubleLess,
    TripleLess,
    DoubleLessDash,
    LessGreater,
    FdDup,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub name: CompactString,
    /// Optional subscript for `name[sub]=value`.
    pub subscript: Option<CompactString>,
    pub op: AssignOp,
    pub value: Option<Word>,
    /// Array literal: `name=(word1 word2 ...)`.
    pub array_value: Option<Vec<Word>>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Append,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Subshell {
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BraceGroup {
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IfClause {
    pub condition: Vec<CompleteCommand>,
    pub then_body: Vec<CompleteCommand>,
    pub elifs: Vec<(Vec<CompleteCommand>, Vec<CompleteCommand>)>,
    pub else_body: Option<Vec<CompleteCommand>>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForClause {
    pub var: CompactString,
    pub words: Option<Vec<Word>>,
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhileClause {
    pub condition: Vec<CompleteCommand>,
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UntilClause {
    pub condition: Vec<CompleteCommand>,
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseClause {
    pub word: Word,
    pub items: Vec<CaseItem>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseItem {
    pub patterns: Vec<Word>,
    pub body: Vec<CompleteCommand>,
    pub terminator: CaseTerminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseTerminator {
    DoubleSemi,
    SemiAnd,
    SemiPipe,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectClause {
    pub var: CompactString,
    pub words: Option<Vec<Word>>,
    pub body: Vec<CompleteCommand>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    pub name: CompactString,
    pub body: Command,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Coproc {
    pub name: Option<CompactString>,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeClause {
    pub pipeline: Pipeline,
}
