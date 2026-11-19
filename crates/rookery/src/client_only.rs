//! rookery on a system other than Linux, where it is a client of a rookery
//! serve elsewhere: every command but those of the server's own machine.

use crate::Result;
use crate::cli::args::{Global, Ssh};
use crate::cli::fail::{Fail, Result as Done};

/// Only rookery on Linux answers for ssh.
pub fn askpass(_: &[String]) -> Result<bool> {
    Ok(false)
}

/// No server runs here, so a shell goes through the server's terminals.
pub fn ssh(global: &Global, ssh: &Ssh) -> Done<()> {
    crate::cli::things::ssh(&crate::cli::client::Client::open(global)?, ssh)
}

/// Only a rookery for Linux goes into a server's image.
pub fn server_context(machine: &str) -> Done<(Vec<u8>, String)> {
    let file = match machine.trim() {
        "aarch64" | "arm64" => "rookery-aarch64-linux.tar.gz",
        _ => "rookery-x86_64-linux.tar.gz",
    };
    Err(Fail::refused(format!(
        "This rookery is for {}, and a server's image holds rookery for Linux. Run it from rookery {} for Linux, from {file} of https://github.com/aiviarium/rookery/releases/tag/v{}",
        std::env::consts::OS,
        rookery_api::VERSION,
        rookery_api::VERSION
    )))
}
