use rookeryd::{Mode, State};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::ExitCode,
};

const USAGE: &str = "rookeryd: the API of a rookery host.

Usage:
  rookeryd                     run as the system service: configuration in /etc/rookery,
                             sockets in /run/rookery, state in /var/lib/rookeryd
  rookeryd --single DIR        run as yourself with everything under DIR (sockets,
                             state, and configuration in DIR/etc when that exists,
                             else /etc/rookery); for containers and tests
  rookeryd connect [--dir DIR] join standard input and output to the control socket
  rookeryd openapi | asyncapi  print the API documents
  rookeryd --version";

/// Copy standard input and output to the control socket until either side closes.
fn connect(dir: PathBuf) -> ExitCode {
    let path = dir.join(rookery_api::CONTROL_SOCKET);
    let stream = match std::os::unix::net::UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("rookeryd: {}: {error}", path.display());
            return ExitCode::from(1);
        }
    };
    let mut to_socket = stream.try_clone().expect("a socket can be cloned");
    std::thread::spawn(move || {
        let mut buffer = [0; 16384];
        let mut input = std::io::stdin().lock();
        while let Ok(count) = input.read(&mut buffer)
            && count > 0
            && to_socket.write_all(&buffer[..count]).is_ok()
        {}
        // No more requests will come; what is still being answered gets out.
        let _ = to_socket.shutdown(std::net::Shutdown::Write);
    });
    let mut from_socket = stream;
    let mut output = std::io::stdout().lock();
    let mut buffer = [0; 16384];
    while let Ok(count) = from_socket.read(&mut buffer)
        && count > 0
        && output
            .write_all(&buffer[..count])
            .and_then(|()| output.flush())
            .is_ok()
    {}
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let run_dir = || {
        PathBuf::from(std::env::var("ROOKERY_DIR").unwrap_or_else(|_| rookery_api::RUN_DIR.into()))
    };
    let (mode, run, config, state) = match words.as_slice() {
        [] => (
            Mode::System,
            PathBuf::from(rookery_api::RUN_DIR),
            PathBuf::from("/etc/rookery"),
            PathBuf::from("/var/lib/rookeryd"),
        ),
        ["--single", dir] => {
            let dir = PathBuf::from(dir);
            let own = dir.join("etc");
            let config = if own.is_dir() {
                own
            } else {
                PathBuf::from("/etc/rookery")
            };
            (Mode::Single, dir.clone(), config, dir.join("state"))
        }
        ["connect"] => return connect(run_dir()),
        ["connect", "--dir", dir] => return connect(PathBuf::from(dir)),
        ["files-helper", ..] => return ExitCode::from(rookeryd::helper_main(&args[1..]) as u8),
        ["openapi"] => {
            println!(
                "{}",
                serde_json::to_string_pretty(&rookeryd::openapi()).expect("a document")
            );
            return ExitCode::SUCCESS;
        }
        ["operations"] => {
            println!(
                "{}",
                serde_json::to_string_pretty(&rookeryd::operations()).expect("a document")
            );
            return ExitCode::SUCCESS;
        }
        ["asyncapi"] => {
            println!(
                "{}",
                serde_json::to_string_pretty(&rookeryd::asyncapi()).expect("a document")
            );
            return ExitCode::SUCCESS;
        }
        ["--version"] => {
            println!(
                "rookeryd {} (api {})",
                rookery_api::VERSION,
                rookery_api::API
            );
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let outcome = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .and_then(|runtime| {
            runtime.block_on(async {
                let state = State::new(mode, config, state)?;
                rookeryd::serve(state, &run).await
            })
        });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rookeryd: {error}");
            ExitCode::from(1)
        }
    }
}
