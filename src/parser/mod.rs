//! The parser: turns a series of tokens into an AST

pub mod intoactualslowtree;
// only `impl`s a function, no need to expose the module
mod display;

use crate::{
    tokenizer::{Interpol as TokenInterpol, Meta, Span, Token},
    utils::stack::Stack,
    value::Value
};
pub use crate::utils::arena::{Arena, NodeId};

const OR: &'static str = "or";

/// An error that occured during parsing
#[derive(Clone, Debug, Fail, PartialEq)]
pub enum ParseError {
    #[fail(display = "can't bind pattern here, already bound before")]
    AlreadyBound,
    #[fail(display = "expected {:?}, found {:?}", _0, _1)]
    Expected(Token, Option<Token>),
    #[fail(display = "expected {}, found {:?}", _0, _1)]
    ExpectedType(&'static str, Token),
    #[fail(display = "invalid type! expected {}", _0)]
    InvalidType(&'static str),
    #[fail(display = "unexpected eof")]
    UnexpectedEOF,
    #[fail(display = "unexpected token {:?} not applicable in this context", _0)]
    Unexpected(Token)
}

/// An AST with the arena and node
pub struct AST<'a> {
    pub arena: Arena<'a, ASTNode>,
    pub root: ASTNode
}

/// An AST node, with metadata
#[derive(Clone, Debug, PartialEq)]
pub struct ASTNode(pub Span, pub ASTType);
/// An AST node type
#[derive(Clone, Debug, PartialEq)]
pub enum ASTType {
    // Types
    Interpol {
        meta: Meta,
        multiline: bool,
        parts: Vec<Interpol>
    },
    Lambda(LambdaArg, Meta, NodeId),
    List(Meta, Vec<NodeId>, Meta),
    Parens(Parens),
    Set {
        recursive: Option<Meta>,
        values: Brackets<Vec<SetEntry>>
    },
    Value(Meta, Value),
    Var(Meta, String),

    // Expressions
    Assert(Meta, NodeId, Meta, NodeId),
    IfElse {
        if_meta: Meta,
        condition: NodeId,
        then_meta: Meta,
        then_body: NodeId,
        else_meta: Meta,
        else_body: NodeId
    },
    Import(Meta, NodeId),
    Let(Meta, Brackets<Vec<SetEntry>>),
    LetIn(Meta, Vec<SetEntry>, Meta, NodeId),
    With(Meta, NodeId, Meta, NodeId),

    // Operators
    Apply(NodeId, NodeId),
    Dynamic {
        meta: Meta,
        ast: NodeId,
        close: Meta
    },
    IndexSet(NodeId, Meta, NodeId),
    Unary(Meta, Unary, NodeId),
    OrDefault {
        set: NodeId,
        dot: Meta,
        attr: NodeId,
        or: Meta,
        default: NodeId
    },

    Operation(NodeId, (Meta, Operator), NodeId),
}
/// An attribute path, a series of ASTs (because dynamic attributes) for the
/// identifiers and metadata for the separators.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute(Vec<(NodeId, Option<Meta>)>);
/// Brackets around something
#[derive(Clone, Debug, PartialEq)]
pub struct Brackets<T>(Meta, T, Meta);
/// A lambda argument type
#[derive(Clone, Debug, PartialEq)]
pub enum LambdaArg {
    Ident(Meta, String),
    Pattern {
        args: Brackets<Vec<PatEntry>>,
        bind: Option<PatternBind>,
        ellipsis: Option<Meta>
    }
}
/// An interpolation part
#[derive(Clone, Debug, PartialEq)]
pub enum Interpol {
    Literal(String),
    AST(NodeId, Meta)
}
/// An operator, such as + - * /
#[derive(Clone, Debug, PartialEq)]
pub enum Operator {
    Concat,
    Merge,
    Add,
    Sub,
    Mul,
    Div,
    And,
    Equal,
    Implication,
    IsSet,
    Less,
    LessOrEq,
    More,
    MoreOrEq,
    NotEqual,
    Or
}
/// An unary operator, such as - and !
#[derive(Clone, Debug, PartialEq)]
pub enum Unary {
    Invert,
    Negate
}
/// Parenthesis around an AST node
#[derive(Clone, Debug, PartialEq)]
pub struct Parens(Meta, NodeId, Meta);
/// An entry in a pattern
#[derive(Clone, Debug, PartialEq)]
pub struct PatEntry {
    pub ident: Meta,
    pub name: String,
    pub default: Option<(Meta, NodeId)>,
    pub comma: Option<Meta>
}
/// A binding for a lambda pattern
#[derive(Clone, Debug, PartialEq)]
pub struct PatternBind {
    pub before: bool,
    pub span: Span,
    pub at: Meta,
    pub ident: Meta,
    pub name: String
}
/// An entry in a set
#[derive(Clone, Debug, PartialEq)]
pub enum SetEntry {
    Assign(Attribute, Meta, NodeId, Meta),
    Inherit(Meta, Option<Parens>, Vec<(Meta, String)>, Meta)
}

type Error = (Option<Span>, ParseError);
type Result<T> = std::result::Result<T, Error>;

macro_rules! math {
    (only_once, $self:expr, $next:block, $($token:pat $(if $cond:expr)* => $op:expr),*) => {{
        let val = { $next };
        Ok(match $self.peek() {
            $(Some(&$token) $(if $cond)* => {
                let (meta, _) = $self.next().unwrap();
                let expr = { $next };
                ASTNode(
                    val.0.until(expr.0),
                    ASTType::Operation($self.insert(val), (meta, $op), $self.insert(expr))
                )
            },)*
            _ => val
        })
    }};
    ($self:expr, $next:block, $($token:pat $(if $cond:expr)* => $op:expr),*) => {{
        let mut val = { $next };
        loop {
            match $self.peek() {
                $(Some(&$token) $(if $cond)* => {
                    let (meta, _) = $self.next().unwrap();
                    let expr = { $next };
                    val = ASTNode(
                        val.0.until(expr.0).into(),
                        ASTType::Operation($self.insert(val), (meta, $op), $self.insert(expr))
                    );
                },)*
                _ => break
            }
        }
        Ok(val)
    }};
}

/// The parser. You may want to use the `parse` convenience function from this module instead.
pub struct Parser<'a, I>
    where I: Iterator<Item = (Meta, Token)>
{
    iter: I,
    buffer: Stack<I::Item>,
    arena: Arena<'a, ASTNode>
}
impl<'a, I> Parser<'a, I>
    where I: Iterator<Item = (Meta, Token)>
{
    /// Create a new instance
    pub fn new(iter: I) -> Self {
        Self::with_arena(Arena::new(), iter)
    }
    /// Create a new instance with a specified arena
    pub fn with_arena(arena: Arena<'a, ASTNode>, iter: I) -> Self {
        Self {
            iter,
            // Can't use [None; 2] because I::Item isn't Copy
            buffer: Stack::new([None, None]),
            arena
        }
    }
    /// Return a reference to the inner arena
    pub fn arena(&self) -> &Arena<'a, ASTNode> {
        &self.arena
    }
    /// Return the owned inner arena
    pub fn into_arena(self) -> Arena<'a, ASTNode> {
        self.arena
    }

    fn parse_branch<T>(&mut self, iter: T) -> Result<ASTNode>
        where T: IntoIterator<Item = (Meta, Token)>
    {
        Parser::with_arena(self.arena.reference(), iter.into_iter())
            .parse_expr()
    }
    fn insert(&mut self, node: ASTNode) -> NodeId {
        self.arena.insert(node)
    }

    fn peek_meta(&mut self) -> Option<&(Meta, Token)> {
        if self.buffer.is_empty() {
            *self.buffer.first_free() = self.iter.next();
        }
        self.buffer.peek()
    }
    fn peek(&mut self) -> Option<&Token> {
        self.peek_meta().map(|(_, token)| token)
    }
    fn next(&mut self) -> Result<I::Item> {
        self.buffer.pop()
            .or_else(|| self.iter.next())
            .ok_or((None, ParseError::UnexpectedEOF))
    }
    fn expect(&mut self, expected: Token) -> Result<Meta> {
        if let Ok((meta, actual)) = self.next() {
            if actual == expected {
                Ok(meta)
            } else {
                Err((Some(meta.span), ParseError::Expected(expected, Some(actual))))
            }
        } else {
            Err((None, ParseError::Expected(expected, None)))
        }
    }


    fn parse_interpol(&mut self, meta: Meta, multiline: bool, values: Vec<TokenInterpol>) -> Result<ASTType> {
        let mut parsed = Vec::new();
        for value in values {
            parsed.push(match value {
                TokenInterpol::Literal(text) => Interpol::Literal(text),
                TokenInterpol::Tokens(tokens, close) => {
                    let parsed = self.parse_branch(tokens)?;
                    Interpol::AST(
                        self.insert(parsed),
                        close
                    )
                }
            });
        }
        Ok(ASTType::Interpol {
            meta,
            multiline,
            parts: parsed
        })
    }
    fn next_attr(&mut self) -> Result<ASTNode> {
        match self.next()? {
            (meta, Token::Ident(ident)) => Ok(ASTNode(meta.span, ASTType::Var(meta, ident))),
            (meta, Token::Value(value)) => Ok(ASTNode(meta.span, ASTType::Value(meta, value))),
            (meta, Token::Dynamic(values, close)) => {
                let parsed = self.parse_branch(values)?;
                Ok(ASTNode(meta.span, ASTType::Dynamic {
                    meta,
                    ast: self.insert(parsed),
                    close
                }))
            },
            (meta, Token::Interpol { multiline, parts }) => Ok(ASTNode(
                meta.span,
                self.parse_interpol(meta, multiline, parts)?
            )),
            (meta, token) => Err((Some(meta.span), ParseError::ExpectedType("attribute", token)))
        }
    }
    fn parse_attr(&mut self) -> Result<Attribute> {
        let mut path = Vec::with_capacity(1);
        loop {
            let attr = self.next_attr()?;
            let attr = self.insert(attr);
            if self.peek() == Some(&Token::Dot) {
                let (dot, _) = self.next().unwrap();
                path.push((attr, Some(dot)));
            } else {
                path.push((attr, None));
                break;
            }
        }
        Ok(Attribute(path))
    }
    fn next_ident(&mut self) -> Result<(Meta, String)> {
        match self.next()? {
            (meta, Token::Ident(name)) => Ok((meta, name)),
            (meta, token) => Err((Some(meta.span), ParseError::ExpectedType("ident", token)))
        }
    }
    fn parse_pattern(&mut self, open: Meta, mut bind: Option<PatternBind>) -> Result<ASTNode> {
        let start = bind.as_ref().map(|bind| bind.span).unwrap_or(open.span);

        let mut args = Vec::with_capacity(1);
        let mut ellipsis = None;
        loop {
            let (ident, name) = match self.peek_meta() {
                Some((_, Token::Ellipsis)) => {
                    let (new, _) = self.next().unwrap();
                    ellipsis = Some(new);
                    break;
                },
                Some((_, Token::CurlyBClose)) => break,
                _ => self.next_ident()?,
            };
            let default = if self.peek() == Some(&Token::Question) {
                let (question, _) = self.next().unwrap();
                let expr = self.parse_expr()?;
                Some((question, self.insert(expr)))
            } else {
                None
            };
            let comma = match self.peek() {
                Some(Token::Comma) => Some(self.next().unwrap().0),
                _ => None
            };
            let no_comma = comma.is_none();
            args.push(PatEntry {
                ident,
                name,
                default,
                comma
            });
            if no_comma {
                break;
            }
        }

        let close = self.expect(Token::CurlyBClose)?;

        if let Some(Token::At) = self.peek() {
            let (at, _) = self.next().unwrap();
            if bind.is_some() {
                return Err((Some(at.span), ParseError::AlreadyBound));
            }
            let (ident, name) = self.next_ident()?;
            bind = Some(PatternBind {
                before: false,
                span: at.span.until(ident.span),
                at,
                ident,
                name
            });
        }

        let colon = self.expect(Token::Colon)?;
        let expr = self.parse_expr()?;

        Ok(ASTNode(start.until(expr.0), ASTType::Lambda(
            LambdaArg::Pattern {
                args: Brackets(open, args, close),
                bind,
                ellipsis
            },
            colon,
            self.insert(expr)
        )))
    }
    fn parse_set(&mut self, until: &Token) -> Result<(Meta, Vec<SetEntry>)> {
        let mut values = Vec::new();
        loop {
            match self.peek() {
                token if token == Some(until) => break,
                Some(Token::Inherit) => {
                    let (meta, _) = self.next().unwrap();

                    let from = if self.peek() == Some(&Token::ParenOpen) {
                        let (open, _) = self.next().unwrap();
                        let from = self.parse_expr()?;
                        let close = self.expect(Token::ParenClose)?;
                        Some(Parens(open, self.insert(from), close))
                    } else {
                        None
                    };

                    let mut vars = Vec::new();
                    while let Some(Token::Ident(_)) = self.peek() {
                        vars.push(self.next_ident().unwrap());
                    }
                    let semi = self.expect(Token::Semicolon)?;

                    values.push(SetEntry::Inherit(meta, from, vars, semi));
                },
                _ => {
                    let key = self.parse_attr()?;
                    let assign = self.expect(Token::Assign)?;
                    let value = self.parse_expr()?;
                    let semi = self.expect(Token::Semicolon)?;

                    values.push(SetEntry::Assign(key, assign, self.insert(value), semi));
                }
            }
        }
        let (end, _) = self.next().unwrap(); // Won't break until reached
        Ok((end, values))
    }
    fn parse_val(&mut self) -> Result<ASTNode> {
        let mut val = match self.next()? {
            (open, Token::ParenOpen) => {
                let expr = self.parse_expr()?;
                let close = self.expect(Token::ParenClose)?;
                ASTNode(
                    open.span.until(close.span),
                    ASTType::Parens(Parens(open, self.insert(expr), close))
                )
            },
            (import, Token::Import) => {
                let value = self.parse_val()?;
                ASTNode(import.span.until(value.0), ASTType::Import(import, self.insert(value)))
            },
            (rec, Token::Rec) => {
                let open = self.expect(Token::CurlyBOpen)?;
                let (close, values) = self.parse_set(&Token::CurlyBClose)?;
                ASTNode(rec.span.until(close.span), ASTType::Set {
                    recursive: Some(rec),
                    values: Brackets(open, values, close)
                })
            },
            (open, Token::CurlyBOpen) => {
                let temporary = self.next()?;
                match (&temporary.1, self.peek()) {
                    (Token::Ident(_), Some(Token::Comma))
                            | (Token::Ident(_), Some(Token::Question))
                            | (Token::Ellipsis, Some(Token::CurlyBClose))
                            | (Token::Ident(_), Some(Token::CurlyBClose))
                            | (Token::CurlyBClose, Some(Token::Colon))
                            | (Token::CurlyBClose, Some(Token::At)) => {
                        // We did a lookahead, put it back
                        self.buffer.push(temporary);
                        self.parse_pattern(open, None)?
                    },
                    _ => {
                        // We did a lookahead, put it back
                        self.buffer.push(temporary);

                        let (close, values) = self.parse_set(&Token::CurlyBClose)?;
                        ASTNode(open.span.until(close.span), ASTType::Set {
                            recursive: None,
                            values: Brackets(open, values, close)
                        })
                    }
                }
            },
            (open, Token::SquareBOpen) => {
                let mut values = Vec::new();
                loop {
                    let peek = self.peek();
                    match peek {
                        None | Some(Token::SquareBClose) => break,
                        _ => {
                            let val = self.parse_val()?;
                            values.push(self.insert(val));
                        }
                    }
                }
                let close = self.expect(Token::SquareBClose)?;
                ASTNode(open.span.until(close.span), ASTType::List(open, values, close))
            },
            (meta, Token::Dynamic(values, close)) => {
                let parsed = self.parse_branch(values)?;
                ASTNode(meta.span, ASTType::Dynamic {
                    meta: meta,
                    ast: self.insert(parsed),
                    close
                })
            },
            (meta, Token::Value(val)) => ASTNode(meta.span, ASTType::Value(meta, val)),
            (meta, Token::Ident(name)) => if self.peek() == Some(&Token::At) {
                let (at, _) = self.next().unwrap();
                let open = self.expect(Token::CurlyBOpen)?;
                self.parse_pattern(open, Some(PatternBind {
                    before: true,
                    span: meta.span.until(at.span),
                    at,
                    ident: meta,
                    name
                }))?
            } else {
                ASTNode(meta.span, ASTType::Var(meta, name))
            },
            (meta, Token::Interpol { multiline, parts }) => ASTNode(
                meta.span,
                self.parse_interpol(meta, multiline, parts)?
            ),
            (meta, token) => return Err((Some(meta.span), ParseError::Unexpected(token)))
        };

        while self.peek() == Some(&Token::Dot) {
            let (dot, _) = self.next().unwrap();
            let attr = self.next_attr()?;
            match self.peek() {
                Some(Token::Ident(s)) if s == OR => {
                    let (or, _) = self.next().unwrap();
                    let default = self.parse_val()?;
                    val = ASTNode(
                        val.0.until(attr.0).into(),
                        ASTType::OrDefault {
                            set: self.insert(val),
                            dot,
                            attr: self.insert(attr),
                            or,
                            default: self.insert(default),
                        }
                    );
                },
                _ => val = ASTNode(
                    val.0.until(attr.0).into(),
                    ASTType::IndexSet(self.insert(val), dot, self.insert(attr))
                )
            }
        }

        Ok(val)
    }
    fn parse_fn(&mut self) -> Result<ASTNode> {
        let mut val = self.parse_val()?;

        while self.peek().map(|t| t.is_fn_arg()).unwrap_or(false) {
            let arg = self.parse_val()?;
            val = ASTNode(
                val.0.until(arg.0).into(),
                ASTType::Apply(self.insert(val), self.insert(arg))
            );
        }

        Ok(val)
    }
    fn parse_negate(&mut self) -> Result<ASTNode> {
        if self.peek() == Some(&Token::Sub) {
            let (sub, _) = self.next().unwrap();
            let expr = self.parse_negate()?;
            Ok(ASTNode(sub.span.until(expr.0), ASTType::Unary(sub, Unary::Negate, self.insert(expr))))
        } else {
            self.parse_fn()
        }
    }
    fn parse_isset(&mut self) -> Result<ASTNode> {
        math!(self, { self.parse_negate()? }, Token::Question => Operator::IsSet)
    }
    fn parse_concat(&mut self) -> Result<ASTNode> {
        math!(self, { self.parse_isset()? }, Token::Concat => Operator::Concat)
    }
    fn parse_mul(&mut self) -> Result<ASTNode> {
        math!(
            self, { self.parse_concat()? },
            Token::Mul => Operator::Mul,
            Token::Div => Operator::Div
        )
    }
    fn parse_add(&mut self) -> Result<ASTNode> {
        math!(
            self, { self.parse_mul()? },
            Token::Add => Operator::Add,
            Token::Sub => Operator::Sub
        )
    }
    fn parse_invert(&mut self) -> Result<ASTNode> {
        if self.peek() == Some(&Token::Invert) {
            let (excl, _) = self.next().unwrap();
            let expr = self.parse_invert()?;
            Ok(ASTNode(excl.span.until(expr.0), ASTType::Unary(excl, Unary::Invert, self.insert(expr))))
        } else {
            self.parse_add()
        }
    }
    fn parse_merge(&mut self) -> Result<ASTNode> {
        math!(self, { self.parse_invert()? }, Token::Merge => Operator::Merge)
    }
    fn parse_compare(&mut self) -> Result<ASTNode> {
        math!(
            only_once, self, { self.parse_merge()? },
            Token::Less => Operator::Less,
            Token::LessOrEq => Operator::LessOrEq,
            Token::More => Operator::More,
            Token::MoreOrEq => Operator::MoreOrEq
        )
    }
    fn parse_equal(&mut self) -> Result<ASTNode> {
        math!(
            only_once, self, { self.parse_compare()? },
            Token::Equal => Operator::Equal,
            Token::NotEqual => Operator::NotEqual
        )
    }
    fn parse_and(&mut self) -> Result<ASTNode> {
        math!(self, { self.parse_equal()? }, Token::And => Operator::And)
    }
    fn parse_or(&mut self) -> Result<ASTNode> {
        math!(self, { self.parse_and()? }, Token::Or => Operator::Or)
    }
    fn parse_implication(&mut self) -> Result<ASTNode> {
        math!(
            self, { self.parse_or()? },
            Token::Implication => Operator::Implication
        )
    }
    #[inline(always)]
    fn parse_math(&mut self) -> Result<ASTNode> {
        // Always point this to the lowest-level math function there is
        self.parse_implication()
    }
    /// Parse Nix code into an AST
    pub fn parse_expr(&mut self) -> Result<ASTNode> {
        Ok(match self.peek() {
            Some(Token::Let) => {
                let (let_, _) = self.next().unwrap();
                if self.peek() == Some(&Token::CurlyBOpen) {
                    let (open, _) = self.next().unwrap();
                    let (close, vars) = self.parse_set(&Token::CurlyBClose)?;
                    ASTNode(
                        let_.span.until(close.span),
                        ASTType::Let(let_, Brackets(open, vars, close))
                    )
                } else {
                    let (in_, vars) = self.parse_set(&Token::In)?;
                    let expr = self.parse_expr()?;
                    ASTNode(
                        let_.span.until(expr.0),
                        ASTType::LetIn(let_, vars, in_, self.insert(expr))
                    )
                }
            },
            Some(Token::With) => {
                let (with, _) = self.next().unwrap();
                let vars = self.parse_expr()?;
                let semi = self.expect(Token::Semicolon)?;
                let rest = self.parse_expr()?;
                ASTNode(
                    with.span.until(rest.0),
                    ASTType::With(with, self.insert(vars), semi, self.insert(rest))
                )
            },
            Some(Token::If) => {
                let (if_meta, _) = self.next().unwrap();
                let condition = self.parse_expr()?;
                let then_meta = self.expect(Token::Then)?;
                let body = self.parse_expr()?;
                let else_meta = self.expect(Token::Else)?;
                let otherwise = self.parse_expr()?;
                ASTNode(
                    if_meta.span.until(otherwise.0).into(),
                    ASTType::IfElse {
                        if_meta,
                        condition: self.insert(condition),
                        then_meta,
                        then_body: self.insert(body),
                        else_meta,
                        else_body: self.insert(otherwise),
                    }
                )
            },
            Some(Token::Assert) => {
                let (assert, _) = self.next().unwrap();
                let condition = self.parse_expr()?;
                let semi = self.expect(Token::Semicolon)?;
                let rest = self.parse_expr()?;
                ASTNode(
                    assert.span.until(rest.0),
                    ASTType::Assert(assert, self.insert(condition), semi, self.insert(rest))
                )
            },
            _ => match self.parse_math()? {
                ASTNode(start, ASTType::Var(meta, name)) => if self.peek() == Some(&Token::Colon) {
                    let (colon, _) = self.next().unwrap();
                    let expr = self.parse_expr()?;
                    ASTNode(
                        start.until(expr.0),
                        ASTType::Lambda(
                            LambdaArg::Ident(meta, name),
                            colon,
                            self.insert(expr)
                        )
                    )
                } else {
                    ASTNode(start, ASTType::Var(meta, name))
                },
                ast => ast
            }
        })
    }
}

/// Convenience function for turning an iterator of tokens into an AST
pub fn parse<I>(iter: I) -> Result<AST<'static>>
    where I: IntoIterator<Item = (Meta, Token)>
{
    let mut parser = Parser::new(iter.into_iter());
    let ast = parser.parse_expr()?;

    Ok(AST {
        arena: parser.into_arena(),
        root: ast
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        tokenizer::{Interpol as TokenInterpol, Meta, Span, Token, Trivia},
        value::{Anchor, Value}
    };
    use super::{intoactualslowtree::*, ASTNode as ASTSpan, ASTType, NodeId, OR, ParseError};

    macro_rules! parse {
        ($($token:expr),*) => {
            super::parse(vec![$((Meta::default(), $token)),*])
                .map(|mut ast| AST::into_tree(ast.root, &mut ast.arena))
        };
    }

    #[test]
    fn set() {
        assert_eq!(
            parse![
                Token::CurlyBOpen,

                Token::Ident("meaning_of_life".into()), Token::Assign, Token::Value(42.into()), Token::Semicolon,
                Token::Ident("H4X0RNUM83R".into()), Token::Assign, Token::Value(1.337.into()), Token::Semicolon,

                Token::CurlyBClose
            ],
            Ok(AST::Set {
                recursive: false,
                values: vec![
                    SetEntry::Assign(vec![AST::Var("meaning_of_life".into())], AST::Value(42.into())),
                    SetEntry::Assign(vec![AST::Var("H4X0RNUM83R".into())], AST::Value(1.337.into()))
                ]
            })
        );
        assert_eq!(
            parse![
                Token::Rec, Token::CurlyBOpen,
                Token::Ident("test".into()), Token::Assign, Token::Value(1.into()), Token::Semicolon,
                Token::CurlyBClose
            ],
            Ok(AST::Set {
                recursive: true,
                values: vec![SetEntry::Assign(vec![AST::Var("test".into())], AST::Value(1.into()))]
            })
        );
        assert_eq!(
            parse![Token::CurlyBOpen, Token::CurlyBClose],
            Ok(AST::Set {
                recursive: false,
                values: Vec::new()
            })
        );
        assert_eq!(
            parse![
                Token::CurlyBOpen,

                Token::Ident("a".into()),
                    Token::Dot, Token::Value("b".into()),
                Token::Assign, Token::Value(1.into()), Token::Semicolon,

                Token::Interpol { multiline: false, parts: vec![TokenInterpol::Literal("c".into())] },
                    Token::Dot, Token::Dynamic(vec![(Meta::default(), Token::Ident("d".into()))], Meta::default()),
                Token::Assign, Token::Value(2.into()), Token::Semicolon,

                Token::CurlyBClose
            ],
            Ok(AST::Set {
                recursive: false,
                values: vec![
                    SetEntry::Assign(vec![
                        AST::Var("a".into()),
                        AST::Value("b".into()),
                    ], AST::Value(1.into())),
                    SetEntry::Assign(vec![
                        AST::Interpol { multiline: false, parts: vec![Interpol::Literal("c".into())] },
                        AST::Dynamic(Box::new(AST::Var("d".into())))
                    ], AST::Value(2.into()))
                ]
            })
        );
    }
    #[test]
    fn meta() {
        let ast = super::parse(vec![
            (Meta::default(), Token::CurlyBOpen),
            (meta! { start: 1, end: 2 }, Token::Semicolon),
        ]);
        assert_eq!(
            ast.map(|_| ()),
            Err((
                Some(Span { start: 1, end: Some(2) }),
                ParseError::ExpectedType("attribute", Token::Semicolon)
            ))
        );
        let ast = super::parse(vec![
            // 1 + /*Hello World*/ 2 * 3
            (meta! { start: 0, end: 1, trailing: 1 }, Token::Value(1.into())),
            (meta! { start: 2, end: 3, trailing: 1 }, Token::Add),
            (
                Meta {
                    span: Span { start: 20, end: Some(21) },
                    leading: vec![
                        Trivia::Comment {
                            span: Span { start: 4, end: Some(19) },
                            multiline: false,
                            content: "Hello World".into()
                        }
                    ],
                    trailing: vec![Trivia::Spaces(1)]
                },
                Token::Value(2.into())
            ),
            (meta! { start: 22, end: 23, trailing: 1 }, Token::Mul),
            (meta! { start: 24, end: 25 }, Token::Value(3.into())),
        ]).unwrap();
        assert_eq!(
            ast.root,
            ASTSpan(
                Span { start: 0, end: Some(25) },
                ASTType::Operation(
                    NodeId(2),
                    (meta! { start: 2, end: 3, trailing: 1 }, Operator::Add),
                    NodeId(3)
                )
            )
        );
        assert_eq!(
            ast.arena.get_ref(),
            &[
                Some(ASTSpan(
                    Span { start: 20, end: Some(21) },
                    ASTType::Value(
                        Meta {
                            span: Span { start: 20, end: Some(21) },
                            leading: vec![
                                Trivia::Comment {
                                    span: Span { start: 4, end: Some(19) },
                                    multiline: false,
                                    content: "Hello World".into()
                                }
                            ],
                            trailing: vec![Trivia::Spaces(1)]
                        },
                        2.into()
                    )
                )),
                Some(ASTSpan(
                    Span { start: 24, end: Some(25) },
                    ASTType::Value(
                        meta! { start: 24, end: 25 },
                        3.into()
                    )
                )),
                Some(ASTSpan(
                    Span { start: 0, end: Some(1) },
                    ASTType::Value(
                        meta! { start: 0, end: 1, trailing: 1 },
                        1.into()
                    )
                )),
                Some(ASTSpan(
                    Span { start: 20, end: Some(25) },
                    ASTType::Operation(
                        NodeId(0),
                        (meta! { start: 22, end: 23, trailing: 1 }, Operator::Mul),
                        NodeId(1)
                    )
                ))
            ]
        );
    }
    #[test]
    fn math() {
        assert_eq!(
            parse![
                Token::Value(1.into()), Token::Add, Token::Value(2.into()), Token::Mul, Token::Value(3.into())
            ],
            Ok(AST::Operation(Operator::Add, Box::new((
                AST::Value(1.into()),
                AST::Operation(Operator::Mul, Box::new((
                    AST::Value(2.into()),
                    AST::Value(3.into()),
                )))
            ))))
        );
        assert_eq!(
            parse![
                Token::Value(5.into()), Token::Mul,
                Token::Sub, Token::ParenOpen,
                    Token::Value(3.into()), Token::Sub, Token::Value(2.into()),
                Token::ParenClose
            ],
            Ok(AST::Operation(Operator::Mul, Box::new((
                AST::Value(5.into()),
                AST::Unary(Unary::Negate, Box::new(AST::Operation(Operator::Sub, Box::new((
                    AST::Value(3.into()),
                    AST::Value(2.into()),
                )))))
            ))))
        );
    }
    #[test]
    fn let_in() {
        assert_eq!(
            parse![
                Token::Let,
                    Token::Ident("a".into()), Token::Assign, Token::Value(42.into()), Token::Semicolon,
                Token::In,
                    Token::Ident("a".into())
            ],
            Ok(AST::LetIn(
                vec![SetEntry::Assign(vec![AST::Var("a".into())], AST::Value(42.into()))],
                Box::new(AST::Var("a".into()))
            ))
        );
    }
    #[test]
    fn let_legacy_syntax() {
        assert_eq!(
            parse![
                Token::Let, Token::CurlyBOpen,
                    Token::Ident("a".into()), Token::Assign, Token::Value(42.into()), Token::Semicolon,
                    Token::Ident("body".into()), Token::Assign, Token::Ident("a".into()), Token::Semicolon,
                Token::CurlyBClose
            ],
            Ok(AST::Let(vec![
                SetEntry::Assign(vec![AST::Var("a".into())], AST::Value(42.into())),
                SetEntry::Assign(vec![AST::Var("body".into())], AST::Var("a".into()))
            ]))
        );
    }
    #[test]
    fn with() {
        assert_eq!(
            parse![
                Token::With, Token::Ident("namespace".into()), Token::Semicolon,
                Token::Ident("expr".into())
            ],
            Ok(AST::With(Box::new((
                AST::Var("namespace".into()),
                AST::Var("expr".into())
            ))))
        );
    }
    #[test]
    fn import() {
        assert_eq!(
            parse![
                Token::Import,
                Token::Value(Value::Path(Anchor::Store, "nixpkgs".into())),
                Token::CurlyBOpen, Token::CurlyBClose
            ],
            Ok(AST::Apply(Box::new((
                AST::Import(Box::new(
                    AST::Value(Value::Path(Anchor::Store, "nixpkgs".into()))
                )),
                AST::Set {
                    recursive: false,
                    values: Vec::new()
                }
            ))))
        );
    }
    #[test]
    fn index_set() {
        assert_eq!(
            parse![
                Token::Ident("a".into()),
                Token::Dot, Token::Ident("b".into()),
                Token::Dot, Token::Ident("c".into())
            ],
            Ok(AST::IndexSet(Box::new((
                AST::IndexSet(Box::new((
                    AST::Var("a".into()),
                    AST::Var("b".into())
                ))),
                AST::Var("c".into())
            ))))
        );
        assert_eq!(
            parse![
                Token::CurlyBOpen,
                    Token::Ident("a".into()),
                        Token::Dot, Token::Ident("b".into()),
                        Token::Dot, Token::Ident("c".into()),
                    Token::Assign, Token::Value(1.into()), Token::Semicolon,
                Token::CurlyBClose
            ],
            Ok(AST::Set {
                recursive: false,
                values: vec![
                    SetEntry::Assign(vec![
                        AST::Var("a".into()),
                        AST::Var("b".into()),
                        AST::Var("c".into())
                    ], AST::Value(1.into()))
                ]
            })
        );
        assert_eq!(
            parse![
                Token::Ident("test".into()),
                    Token::Dot, Token::Value("invalid ident".into()),
                    Token::Dot, Token::Interpol { multiline: false, parts: vec![TokenInterpol::Literal("hi".into())] },
                    Token::Dot, Token::Dynamic(
                        vec![(Meta::default(), Token::Ident("a".into()))],
                        Meta::default()
                    )
            ],
            Ok(AST::IndexSet(Box::new((
                AST::IndexSet(Box::new((
                    AST::IndexSet(Box::new((
                        AST::Var("test".into()),
                        AST::Value("invalid ident".into())
                    ))),
                    AST::Interpol { multiline: false, parts: vec![Interpol::Literal("hi".into())] }
                ))),
                AST::Dynamic(Box::new(AST::Var("a".into())))
            ))))
        );
    }
    #[test]
    fn interpolation() {
        assert_eq!(
            parse![
                Token::Interpol {
                    multiline: false,
                    parts: vec![
                        TokenInterpol::Literal("Hello, ".into()),
                        TokenInterpol::Tokens(
                            vec![
                                (Meta::default(), Token::CurlyBOpen),
                                (Meta::default(), Token::Ident("world".into())),
                                (Meta::default(), Token::Assign),
                                (Meta::default(), Token::Value("World".into())),
                                (Meta::default(), Token::Semicolon),
                                (Meta::default(), Token::CurlyBClose),
                                (Meta::default(), Token::Dot),
                                (Meta::default(), Token::Ident("world".into()))
                            ],
                            Meta::default()
                        ),
                        TokenInterpol::Literal("!".into())
                    ]
                }
            ],
            Ok(AST::Interpol {
                multiline: false,
                parts: vec![
                    Interpol::Literal("Hello, ".into()),
                    Interpol::AST(AST::IndexSet(Box::new((
                        AST::Set {
                            recursive: false,
                            values: vec![SetEntry::Assign(vec![AST::Var("world".into())], AST::Value("World".into()))]
                        },
                        AST::Var("world".into())
                    )))),
                    Interpol::Literal("!".into())
                ]
            })
        );
    }
    #[test]
    fn list() {
        assert_eq!(
            parse![
               Token::SquareBOpen,
               Token::Ident("a".into()), Token::Value(2.into()), Token::Value(3.into()),
               Token::Value("lol".into()),
               Token::SquareBClose
            ],
            Ok(AST::List(vec![
                AST::Var("a".into()), AST::Value(2.into()), AST::Value(3.into()),
                AST::Value("lol".into())
            ]))
        );
        assert_eq!(
            parse![
               Token::SquareBOpen, Token::Value(1.into()), Token::SquareBClose, Token::Concat,
               Token::SquareBOpen, Token::Value(2.into()), Token::SquareBClose, Token::Concat,
               Token::SquareBOpen, Token::Value(3.into()), Token::SquareBClose
            ],
            Ok(AST::Operation(Operator::Concat, Box::new((
                AST::Operation(Operator::Concat, Box::new((
                    AST::List(vec![AST::Value(1.into())]),
                    AST::List(vec![AST::Value(2.into())]),
                ))),
                AST::List(vec![AST::Value(3.into())])
            ))))
        );
    }
    #[test]
    fn functions() {
        assert_eq!(
            parse![
               Token::Ident("a".into()), Token::Colon, Token::Ident("b".into()), Token::Colon,
               Token::Ident("a".into()), Token::Add, Token::Ident("b".into())
            ],
            Ok(AST::Lambda(
                LambdaArg::Ident("a".into()),
                Box::new(AST::Lambda(
                    LambdaArg::Ident("b".into()),
                    Box::new(AST::Operation(Operator::Add, Box::new((
                        AST::Var("a".into()),
                        AST::Var("b".into())
                    ))))
                ))
            ))
        );
        assert_eq!(
            parse![Token::CurlyBOpen, Token::CurlyBClose, Token::Colon, Token::Value(1.into())],
            Ok(AST::Lambda(
                LambdaArg::Pattern {
                    args: Vec::new(),
                    bind: None,
                    ellipsis: false
                },
                Box::new(AST::Value(1.into()))
            ))
        );
        assert_eq!(
            parse![
                Token::CurlyBOpen, Token::CurlyBClose, Token::At, Token::Ident("outer".into()),
                Token::Colon, Token::Value(1.into())
            ],
            Ok(AST::Lambda(
                LambdaArg::Pattern {
                    args: Vec::new(),
                    bind: Some("outer".into()),
                    ellipsis: false
                },
                Box::new(AST::Value(1.into()))
            ))
        );
        assert_eq!(
            parse![Token::CurlyBOpen, Token::Ellipsis, Token::CurlyBClose, Token::Colon, Token::Value(1.into())],
            Ok(AST::Lambda(
                LambdaArg::Pattern {
                    args: Vec::new(),
                    bind: None,
                    ellipsis: true
                },
                Box::new(AST::Value(1.into()))
            ))
        );
        assert_eq!(
            parse![
                Token::Ident("a".into()), Token::Value(1.into()), Token::Value(2.into()),
                Token::Add,
                Token::Value(3.into())
            ],
            Ok(AST::Operation(Operator::Add, Box::new((
                AST::Apply(Box::new((
                    AST::Apply(Box::new((
                        AST::Var("a".into()),
                        AST::Value(1.into())
                    ))),
                    AST::Value(2.into()),
                ))),
                AST::Value(3.into())
            ))))
        );
    }
    #[test]
    fn patterns() {
        assert_eq!(
            parse![
                Token::CurlyBOpen,
                    Token::Ident("a".into()), Token::Comma,
                    Token::Ident("b".into()), Token::Question, Token::Value("default".into()),
                Token::CurlyBClose,
                Token::Colon,
                Token::Ident("a".into())
            ],
            Ok(AST::Lambda(
                LambdaArg::Pattern {
                    args: vec![
                        PatEntry("a".into(), None),
                        PatEntry("b".into(), Some(AST::Value("default".into()))),
                    ],
                    bind: None,
                    ellipsis: false
                },
                Box::new(AST::Var("a".into()))
            ))
        );
        assert_eq!(
            parse![
                Token::CurlyBOpen,
                    Token::Ident("a".into()), Token::Comma,
                    Token::Ident("b".into()), Token::Question, Token::Value("default".into()), Token::Comma,
                    Token::Ellipsis,
                Token::CurlyBClose,
                Token::At,
                Token::Ident("outer".into()),
                Token::Colon,
                Token::Ident("outer".into())
            ],
            Ok(AST::Lambda(
                LambdaArg::Pattern {
                    args: vec![
                        PatEntry("a".into(), None),
                        PatEntry("b".into(), Some(AST::Value("default".into()))),
                    ],
                    bind: Some("outer".into()),
                    ellipsis: true
                },
                Box::new(AST::Var("outer".into()))
            ))
        );
        assert_eq!(
            parse![
                Token::Ident("outer".into()), Token::At,
                Token::CurlyBOpen, Token::Ident("a".into()), Token::CurlyBClose,
                Token::Colon,
                Token::Ident("outer".into())
            ],
            Ok(AST::Lambda(
                LambdaArg::Pattern {
                    args: vec![PatEntry("a".into(), None)],
                    bind: Some("outer".into()),
                    ellipsis: false
                },
                Box::new(AST::Var("outer".into()))
            ))
        );
        assert_eq!(
            parse![
                Token::CurlyBOpen,
                    Token::Ident("a".into()), Token::Question, Token::CurlyBOpen, Token::CurlyBClose,
                Token::CurlyBClose, Token::Colon, Token::Ident("a".into())
            ],
            Ok(AST::Lambda(
                LambdaArg::Pattern {
                    args: vec![PatEntry("a".into(), Some(AST::Set { recursive: false, values: Vec::new() }))],
                    bind: None,
                    ellipsis: false
                },
                Box::new(AST::Var("a".into()))
            ))
        );
    }
    #[test]
    fn merge() {
        assert_eq!(
            parse![
                Token::CurlyBOpen,
                    Token::Ident("a".into()), Token::Assign, Token::Value(1.into()), Token::Semicolon,
                Token::CurlyBClose,
                Token::Merge,
                Token::CurlyBOpen,
                    Token::Ident("b".into()), Token::Assign, Token::Value(2.into()), Token::Semicolon,
                Token::CurlyBClose
            ],
            Ok(AST::Operation(Operator::Merge, Box::new((
                AST::Set {
                    recursive: false,
                    values: vec![SetEntry::Assign(vec![AST::Var("a".into())], AST::Value(1.into()))]
                },
                AST::Set {
                    recursive: false,
                    values: vec![SetEntry::Assign(vec![AST::Var("b".into())], AST::Value(2.into()))]
                }
            ))))
        )
    }
    #[test]
    fn ifs() {
        assert_eq!(
            parse![
                Token::Value(false.into()), Token::Implication,
                Token::Invert, Token::Value(false.into()),
                Token::And,
                Token::Value(false.into()), Token::Equal, Token::Value(true.into()),
                Token::Or,
                Token::Value(true.into())
            ],
            Ok(AST::Operation(Operator::Implication, Box::new((
                AST::Value(false.into()),
                AST::Operation(Operator::Or, Box::new((
                    AST::Operation(Operator::And, Box::new((
                        AST::Unary(Unary::Invert, Box::new(AST::Value(false.into()))),
                        AST::Operation(Operator::Equal, Box::new((
                            AST::Value(false.into()),
                            AST::Value(true.into())
                        )))
                    ))),
                    AST::Value(true.into())
                )))
            ))))
        );
        assert_eq!(
            parse![
                Token::Value(1.into()), Token::Less, Token::Value(2.into()),
                Token::Or,
                Token::Value(2.into()), Token::LessOrEq, Token::Value(2.into()),
                Token::And,
                Token::Value(2.into()), Token::More, Token::Value(1.into()),
                Token::And,
                Token::Value(2.into()), Token::MoreOrEq, Token::Value(2.into())
            ],
            Ok(AST::Operation(Operator::Or, Box::new((
                AST::Operation(Operator::Less, Box::new((
                    AST::Value(1.into()),
                    AST::Value(2.into())
                ))),
                AST::Operation(Operator::And, Box::new((
                    AST::Operation(Operator::And, Box::new((
                        AST::Operation(Operator::LessOrEq, Box::new((
                            AST::Value(2.into()),
                            AST::Value(2.into())
                        ))),
                        AST::Operation(Operator::More, Box::new((
                            AST::Value(2.into()),
                            AST::Value(1.into())
                        ))),
                    ))),
                    AST::Operation(Operator::MoreOrEq, Box::new((
                        AST::Value(2.into()),
                        AST::Value(2.into())
                    )))
                )))
            ))))
        );
        assert_eq!(
            parse![
                Token::Value(1.into()), Token::Equal, Token::Value(1.into()),
                Token::And,
                Token::Value(2.into()), Token::NotEqual, Token::Value(3.into())
            ],
            Ok(AST::Operation(Operator::And, Box::new((
                AST::Operation(Operator::Equal, Box::new((
                    AST::Value(1.into()),
                    AST::Value(1.into())
                ))),
                AST::Operation(Operator::NotEqual, Box::new((
                    AST::Value(2.into()),
                    AST::Value(3.into())
                )))
            ))))
        );
        assert_eq!(
            parse![
                Token::If, Token::Value(false.into()), Token::Then,
                    Token::Value(1.into()),
                Token::Else,
                    Token::If, Token::Value(true.into()), Token::Then,
                        Token::Value(2.into()),
                    Token::Else,
                        Token::Value(3.into())
            ],
            Ok(AST::IfElse(Box::new((
                AST::Value(false.into()),
                AST::Value(1.into()),
                AST::IfElse(Box::new((
                    AST::Value(true.into()),
                    AST::Value(2.into()),
                    AST::Value(3.into())
                )))
            ))))
        )
    }
    #[test]
    fn assert() {
        assert_eq!(
            parse![
                Token::Assert, Token::Ident("a".into()), Token::Equal, Token::Ident("b".into()), Token::Semicolon,
                Token::Value("a == b".into())
            ],
            Ok(AST::Assert(Box::new((
                AST::Operation(Operator::Equal, Box::new((
                    AST::Var("a".into()),
                    AST::Var("b".into())
                ))),
                AST::Value("a == b".into())
            ))))
        );
    }
    #[test]
    fn inherit() {
        assert_eq!(
            parse![
                Token::CurlyBOpen,
                    Token::Ident("a".into()), Token::Assign, Token::Value(1.into()), Token::Semicolon,
                    Token::Inherit, Token::Ident("b".into()), Token::Semicolon,

                    Token::Inherit, Token::ParenOpen, Token::Ident("set".into()), Token::ParenClose,
                    Token::Ident("c".into()), Token::Semicolon,
                Token::CurlyBClose
            ],
            Ok(AST::Set {
                recursive: false,
                values: vec![
                    SetEntry::Assign(vec![AST::Var("a".into())], AST::Value(1.into())),
                    SetEntry::Inherit(None, vec!["b".into()]),
                    SetEntry::Inherit(Some(AST::Var("set".into())), vec!["c".into()]),
                ]
            })
        );
    }
    #[test]
    fn isset() {
        assert_eq!(
            parse![
                Token::Ident("a".into()), Token::Question, Token::Value("b".into()),
                Token::And, Token::Value(true.into())
            ],
            Ok(AST::Operation(Operator::And, Box::new((
                AST::Operation(Operator::IsSet, Box::new((
                    AST::Var("a".into()),
                    AST::Value("b".into())
                ))),
                AST::Value(true.into())
            ))))
        );
        assert_eq!(
            parse![
                Token::Ident("a".into()),
                    Token::Dot, Token::Ident("b".into()),
                    Token::Dot, Token::Ident("c".into()),
                Token::Ident(OR.into()), Token::Value(1.into()),
                Token::Add, Token::Value(1.into())
            ],
            Ok(AST::Operation(Operator::Add, Box::new((
                AST::OrDefault(Box::new((
                    AST::IndexSet(Box::new((
                        AST::Var("a".into()),
                        AST::Var("b".into())
                    ))),
                    AST::Var("c".into()),
                    AST::Value(1.into())
                ))),
                AST::Value(1.into())
            ))))
        );
    }
}
