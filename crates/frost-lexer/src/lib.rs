//! Zsh-compatible lexer.
//!
//! Tokenizes zsh source into a stream of [`Token`]s. The lexer is
//! context-aware: quoting state, heredoc delimiters, and alias
//! expansion all influence tokenization (matching zsh behavior).

mod token;
mod cursor;
mod lexer;

pub use token::{Token, TokenKind, Span};
pub use lexer::Lexer;
