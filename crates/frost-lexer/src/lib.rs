//! Zsh-compatible lexer.
//!
//! Tokenizes zsh source into a stream of [`Token`]s. The lexer is
//! context-aware: quoting state, heredoc delimiters, and alias
//! expansion all influence tokenization (matching zsh behavior).

mod cursor;
mod lexer;
mod nest;
mod token;
mod words;

pub use lexer::{Lexer, tokenize, tokenize_str};
pub use nest::matching_close;
pub use token::{Span, Token, TokenKind};
pub use words::split_words;
