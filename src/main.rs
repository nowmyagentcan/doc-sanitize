use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            let _ = writeln!(
                std::io::stdout(),
                "doc-sanitize {}",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        _ => {
            let _ = writeln!(
                std::io::stderr(),
                "No file given.\nTry: doc-sanitize contract.docx"
            );
            ExitCode::from(2)
        }
    }
}
