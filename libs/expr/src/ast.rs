//! The compiled expression tree.
//!
//! Only the shapes a binding may contain exist here. Everything the parser
//! refuses (calls, assignments, statements, shell and Bus forms, function
//! literals) never gets a node, so the evaluator has nothing to deny at
//! run time.

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Number(f64),
    Str(String),
    Bool(bool),
    Nil,
    /// A double-quoted string with at least one `${...}` part.
    Interp(Vec<Part>),
    /// `$name`: a global supplied by the host.
    Var(String),
    Binary {
        op: BinOp,
        left: Box<Node>,
        right: Box<Node>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Node>,
    },
    /// `cond ? a : b`, right-associative, short-circuit.
    Ternary {
        cond: Box<Node>,
        then: Box<Node>,
        otherwise: Box<Node>,
    },
    /// `if c then a elif d then b else e end` in expression position. Each
    /// branch body is at most one expression; an empty body yields nil, as
    /// does a chain with no taken branch and no `else`.
    If {
        branches: Vec<IfBranch>,
        otherwise: Option<Box<Node>>,
    },
    Field {
        object: Box<Node>,
        field: String,
    },
    Index {
        object: Box<Node>,
        index: Box<Node>,
    },
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct IfBranch {
    pub(crate) cond: Node,
    pub(crate) body: Option<Node>,
}

/// One piece of an interpolated string.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Part {
    Literal(String),
    Var(InterpVar),
}

/// A `${head.field[index] ?? default}` interpolation. The head is a global;
/// the segments are applied in order; the default fires on nil (`??`) or on
/// any falsy value (`?:`), and also when the head is not a known global.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InterpVar {
    pub(crate) head: String,
    pub(crate) segments: Vec<Segment>,
    /// `None` is "no default"; `Some((kind, None))` is an empty default,
    /// which renders as the empty string.
    pub(crate) coalesce: Option<(Coalesce, Option<Box<Node>>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Segment {
    Field(String),
    Index(Node),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Coalesce {
    /// `??`: fires on nil only.
    Nil,
    /// `?:`: fires on any falsy value.
    Falsy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    /// `eq`: textual equality of the rendered forms.
    StrEq,
    /// `ne`: textual inequality of the rendered forms.
    StrNe,
    And,
    Or,
    Concat,
    /// `??`
    NilCoalesce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
    Not,
}
