//! ghp-lsp: tooling for GHP (`.ghp`) templates.
//!
//! Layers, bottom up: [`line_index`] (byte offset <-> LSP UTF-16 position), [`lexer`],
//! [`parser`] (error-tolerant, always yields a [`ast::Tree`]) and [`diagnostics`].
//! [`gopls`] bridges the Go expressions to a real gopls, [`scope`] answers local-name questions
//! without it, and [`router`] is the API a language server calls: it decides whether a position
//! is Go, delegates, and falls back.

pub mod ast;
pub mod diagnostics;
pub mod gopls;
pub mod lexer;
pub mod line_index;
pub mod parser;
pub mod router;
pub mod scope;
pub mod server;
