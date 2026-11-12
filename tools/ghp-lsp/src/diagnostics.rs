//! Diagnostics produced by the lexer and parser.

use crate::ast::Span;
use crate::line_index::{LineIndex, Range};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// Stable machine-readable diagnostic codes.
pub mod code {
    pub const UNTERMINATED_TAG: &str = "unterminated-tag";
    pub const TAG_NOT_CLOSED: &str = "tag-not-closed";
    pub const UNBALANCED_TAG: &str = "unbalanced-tag";
    pub const UNKNOWN_TAG: &str = "unknown-tag";
    pub const EMPTY_ECHO: &str = "empty-echo";
    pub const EMPTY_IF: &str = "empty-if";
    pub const EMPTY_ELIF: &str = "empty-elif";
    pub const EMPTY_FOR: &str = "empty-for";
    pub const EMPTY_CASE: &str = "empty-case";
    pub const ELSE_WITH_PAYLOAD: &str = "else-with-payload";
    pub const DEFAULT_WITH_PAYLOAD: &str = "default-with-payload";
    pub const CLOSER_WITH_PAYLOAD: &str = "closer-with-payload";
    pub const UNCLOSED_IF: &str = "unclosed-if";
    pub const UNCLOSED_SWITCH: &str = "unclosed-switch";
    pub const UNCLOSED_FOR: &str = "unclosed-for";
    pub const UNMATCHED_ENDIF: &str = "unmatched-endif";
    pub const UNMATCHED_ENDFOR: &str = "unmatched-endfor";
    pub const UNMATCHED_ENDSWITCH: &str = "unmatched-endswitch";
    pub const ELIF_OUTSIDE_IF: &str = "elif-outside-if";
    pub const ELSE_OUTSIDE_IF: &str = "else-outside-if";
    pub const CASE_OUTSIDE_SWITCH: &str = "case-outside-switch";
    pub const DEFAULT_OUTSIDE_SWITCH: &str = "default-outside-switch";
    pub const ELIF_AFTER_ELSE: &str = "elif-after-else";
    pub const DUPLICATE_ELSE: &str = "duplicate-else";
    pub const CASE_AFTER_DEFAULT: &str = "case-after-default";
    pub const DUPLICATE_DEFAULT: &str = "duplicate-default";
    pub const SWITCH_BODY: &str = "switch-body";
    pub const NESTED_IMPORT: &str = "nested-import";
    pub const IMPORT_EMPTY: &str = "import-empty";
    pub const IMPORT_PARENS: &str = "import-parens";
    pub const IMPORT_ITEM: &str = "import-item";
    pub const IMPORT_ALIAS: &str = "import-alias";
    pub const IMPORT_PATH: &str = "import-path";
    pub const TOO_DEEP: &str = "too-deep";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

impl Diagnostic {
    pub fn error(span: Span, code: &'static str, message: impl Into<String>) -> Self {
        Diagnostic { span, severity: Severity::Error, code, message: message.into() }
    }

    pub fn warning(span: Span, code: &'static str, message: impl Into<String>) -> Self {
        Diagnostic { span, severity: Severity::Warning, code, message: message.into() }
    }

    /// The LSP range of this diagnostic.
    pub fn range(&self, index: &LineIndex) -> Range {
        index.range(self.span)
    }
}
