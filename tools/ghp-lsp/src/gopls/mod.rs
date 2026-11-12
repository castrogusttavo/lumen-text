//! Bridge to a real `gopls` for the Go expressions inside `.ghp` pages.
//!
//! * [`virtual_doc`]: `.ghp` + expression regions -> virtual `.go` mirroring the transpiler.
//! * [`sourcemap`]: byte / UTF-16 position mapping between the two.
//! * [`proxy`]: gopls child process, JSON-RPC, overlay documents, request/diagnostic translation.
//! * [`adapter`]: the parser's tree -> the regions above.

pub mod adapter;
pub mod proxy;
pub mod sourcemap;
pub mod virtual_doc;

pub use proxy::{Bridge, CancelToken, Config, Diagnostic, Event, ProxyError};
pub use sourcemap::{Position, Range, SourceMap};
pub use virtual_doc::{ExprRegion, Options, RegionKind, VirtualDoc, build};
