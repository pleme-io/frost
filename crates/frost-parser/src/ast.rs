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
    CommandSub(Box<Program>),
    ArithSub(CompactString),
    Glob(GlobKind),
    Tilde(CompactString),
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
    pub op: AssignOp,
    pub value: Option<Word>,
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
