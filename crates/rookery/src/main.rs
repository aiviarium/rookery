//! rookery: set up and look after agent workstations. Its commands are
//! clients of a rookery server's API, on Linux, macOS and Windows. On Linux
//! the program has the server in it as well: `rookery serve`, and one in the
//! process for a command when no other is set.

mod cli;
#[cfg(target_os = "linux")]
mod local;
#[cfg(not(target_os = "linux"))]
#[path = "client_only.rs"]
mod local;
mod update;

use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// A server on this machine, and what to do once it is no longer needed.
type Local = (String, Box<dyn FnOnce()>);

/// End quietly when what reads rookery's output has gone, as `head` does after
/// its lines: printing then panics, which is no fault of rookery's to report.
fn end_quietly_on_a_closed_pipe() {
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let said = panic.payload().downcast_ref::<String>();
        if said.is_some_and(|said| said.starts_with("failed printing to std")) {
            std::process::exit(141);
        }
        report(panic);
    }));
}

fn main() {
    end_quietly_on_a_closed_pipe();
    let args: Vec<String> = std::env::args().collect();
    match local::askpass(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("rookery: {error}");
            std::process::exit(1);
        }
    }
    std::process::exit(cli::main(args));
}
