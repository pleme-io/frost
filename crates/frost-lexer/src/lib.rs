//! Zsh-compatible lexer.
//!
//! Tokenizes zsh source into a stream of [`Token`]s. The lexer is
//! context-aware: quoting state, heredoc delimiters, and alias
//! expansion all influence tokenization (matching zsh behavior).

mod cursor;
mod lexer;
mod token;

pub use lexer::Lexer;
pub use token::{Span, Token, TokenKind};
