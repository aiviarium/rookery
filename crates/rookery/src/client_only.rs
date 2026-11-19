//! rookery on a system other than Linux, where it is a client of a rookery
//! serve elsewhere: every command but those of the server's own machine.

use crate::Result;
use crate::cli::args::{Global, Ssh};
use crate::cli::fail::{Fail, Result as Done};

/// Only rookery on Linux answers for ssh.
pub fn askpass(_: &[String]) -> Result<bool> {
    Ok(false)
}

/// No server runs here: a command needs one named.
pub fn server(_ask: bool) -> Result<Option<crate::Local>> {
    Ok(None)
}

/// No server runs in this process to give a sudo password to.
pub fn give_sudo_password(_: &str, _: &str) -> bool {
    false
}

/// Plain SSH needs the server's key, which is on the machine it runs on.
pub fn ssh(_: &Global, ssh: &Ssh) -> Done<()> {
    Err(Fail::refused(format!(
        "rookery ssh logs in with the server's own key, on the Linux machine it runs on. rookery session attach {} reaches the host through the server",
        ssh.host
    )))
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
