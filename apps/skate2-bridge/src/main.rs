//! `skate2-bridge --listen <ip:port> --server <ip:port>`: WebSocket clients on
//! `--listen`, each bridged to the UDP server at `--server`
//! (`skate2_bridge`). The server address is this process's configuration;
//! pages cannot choose it.

use std::{net::SocketAddr, sync::atomic::AtomicBool};

fn main() -> std::process::ExitCode {
    let mut listen: SocketAddr = ([127, 0, 0, 1], 8787).into();
    let mut server = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .and_then(|value| value.parse::<SocketAddr>().ok());
        match (flag.as_str(), value) {
            ("--listen", Some(addr)) => listen = addr,
            ("--server", Some(addr)) => server = Some(addr),
            _ => {
                eprintln!("usage: skate2-bridge [--listen ip:port] --server ip:port");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let Some(server) = server else {
        eprintln!("usage: skate2-bridge [--listen ip:port] --server ip:port");
        return std::process::ExitCode::FAILURE;
    };
    let listener = match std::net::TcpListener::bind(listen) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skate2-bridge: {listen}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    eprintln!("skate2-bridge: ws://{listen} -> udp {server}");
    match skate2_bridge::serve(&listener, server, &AtomicBool::new(false)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("skate2-bridge: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
