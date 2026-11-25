//! The `matty` binary: parses the command line and starts the app.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

#[cfg(windows)]
use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE};

// Explorer and browser callbacks must not open a console. Explicit CLI
// requests can still write to the console they were launched from.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "attaching to the parent console is a Win32 call"
)]
fn attach_cli_console(output: STD_HANDLE) {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle};

    // SAFETY: both arguments are Win32 constants. The borrowed standard
    // handle is only checked, never closed. Leave redirected output alone.
    unsafe {
        if !GetStdHandle(output).is_ok_and(|handle| !handle.is_invalid()) {
            // Failure is expected if the caller has no console.
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[expect(
    clippy::print_stdout,
    reason = "--version and --help answer on stdout before the app or its logger starts"
)]
fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // A Matty already running takes this launch's link, or only comes
        // forward, and this one exits (#84).
        None if matty_app::instance::forward(None) => ExitCode::SUCCESS,
        None => {
            matty_app::run();
            ExitCode::SUCCESS
        }
        Some("--offline") => {
            matty_app::run_offline();
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            #[cfg(windows)]
            attach_cli_console(STD_OUTPUT_HANDLE);
            println!("matty {}", matty_app::VERSION);
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            #[cfg(windows)]
            attach_cli_console(STD_OUTPUT_HANDLE);
            println!(
                "matty {}\n\nUSAGE:\n    matty [--offline | --version | --help | <link>]\n\n    --offline    open the last account's cache without connecting\n    <link>       open a matty:// or mattermost:// link: a channel, a post, or the SSO hand-off",
                matty_app::VERSION
            );
            ExitCode::SUCCESS
        }
        // A deep link, such as the SSO hand-off a browser passes to
        // `matty mattermost://…` on systems that launch the handler.
        Some(link) if link.contains("://") && matty_app::instance::forward(Some(link)) => {
            ExitCode::SUCCESS
        }
        Some(link) if link.contains("://") => {
            matty_app::run_with_links(vec![link.to_string()]);
            ExitCode::SUCCESS
        }
        Some(other) => {
            #[cfg(windows)]
            attach_cli_console(STD_ERROR_HANDLE);
            eprintln!("matty: unknown argument `{other}`; try `matty --help`");
            ExitCode::from(2)
        }
    }
}
