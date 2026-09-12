//! `hsm setup-chat` — hand the popup over to `agentmail setup`.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use crate::agentmail;
use crate::runtime::Ctx;

pub fn run(ctx: &Ctx) -> ExitCode {
    let Some(bin) = agentmail::find(&ctx.config.agentmail_bin) else {
        print_hint(ctx);
        wait_for_enter();
        return ExitCode::SUCCESS;
    };

    match agentmail::run_setup(&bin) {
        // Unix replaces the process, so only a failure comes back.
        Err(error) => {
            eprintln!("hsm: {error:#}");
            wait_for_enter();
            ExitCode::FAILURE
        }
        #[cfg(unix)]
        Ok(never) => match never {},
        #[cfg(not(unix))]
        Ok(status) => {
            if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn print_hint(ctx: &Ctx) {
    let root = ctx
        .plugin
        .plugin_root
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "this plugin's directory".to_string());

    println!("Agent chat is not set up yet.");
    println!();
    println!("Agent chat lets one agent message another across harnesses. It needs a");
    println!("second binary, `agentmail`, which is not installed here: hsm looked next");
    println!("to itself and on PATH.");
    println!();
    println!("To build it (Rust 1.85 or newer):");
    println!();
    println!("    cd {root}");
    println!("    cargo build --release --bin agentmail");
    println!();
    println!("Then run this action again and it will open the setup checklist.");
    println!();
    print!("Press Enter to close. ");
    let _ = std::io::stdout().flush();
}

/// Without this the popup would close before anyone could read the hint.
fn wait_for_enter() {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}
