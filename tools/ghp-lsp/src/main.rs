//! `ghp-lsp`: stdio language server for GHP (`.ghp`) templates.

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let code = ghp_lsp::server::run(stdin.lock(), stdout.lock());
    std::process::exit(code);
}
