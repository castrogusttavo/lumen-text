//! `ghp-lsp`: stdio language server for GHP (`.ghp`) templates.

use std::io::BufReader;

fn main() {
    let input = BufReader::new(std::io::stdin());
    let code = ghp_lsp::server::run(input, std::io::stdout().lock());
    std::process::exit(code);
}
