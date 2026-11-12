//! ghp-lsp: tooling for GHP (`.ghp`) templates.
//!
//! Layers, bottom up: [`line_index`] (byte offset <-> LSP UTF-16 position), [`lexer`],
//! [`parser`] (error-tolerant, always yields a [`ast::Tree`]) and [`diagnostics`].

pub mod ast;
pub mod diagnostics;
pub mod lexer;
pub mod line_index;
pub mod parser;
// pub mod gopls; // added by the gopls bridge work (src/gopls/)
