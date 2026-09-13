fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--mcp") => {
            if let Err(error) = kokoro_reader_lib::run_mcp() {
                eprintln!("MCP server error: {error}");
                std::process::exit(1);
            }
        }
        Some("--install-codex") => {
            eprintln!(
                "--install-codex is deprecated. Install the repo-local plugin with `codex plugin marketplace add` and `codex plugin add`."
            );
            std::process::exit(2);
        }
        _ => kokoro_reader_lib::run(),
    }
}
