// Port of main.h / main.c
//
// Rust port of https://github.com/ssleert/riffdb. Module layout is 1:1 with
// the C sources; see docs/PORTING.md for the mapping and the list of deliberate
// deviations (marked `PORT FIX` / `PORT NOTE` in code).

#![forbid(unsafe_code)]

mod any;
mod channel;
mod database;
mod execute;
mod greeting;
mod http_parser;
mod http_response;
mod http_utils;
mod log;
mod options;
mod protocol;
mod query;
mod queue;
mod request;
mod router;
mod service;
mod socket_actions;
mod tcp_server;
mod thread_pool;
mod utils;
mod worker;

use std::process::ExitCode;

use crate::database::database_create_if_not_exists;
use crate::greeting::greeting;
use crate::options::{parse_options, print_usage, print_version, set_options};
use crate::socket_actions::SocketActions;
use crate::tcp_server::TcpServer;
use crate::thread_pool::ThreadPool;
use crate::utils::mkdir_if_not_exists;
use crate::worker::worker_handler;

pub const PROGRAM_NAME: &str = "riffdb";
pub const PROGRAM_VERSION: &str = "0.0.1";

fn main() -> ExitCode {
    // C: #ifdef NTRACE (set for Release builds)
    #[cfg(not(debug_assertions))]
    {
        log::set_max_verbosity(log::LogVerbosity::Info);
    }

    if std::env::var_os("NO_COLOR").is_some() {
        log::set_colored(false);
    }

    let argv: Vec<String> = std::env::args().collect();
    let Ok(options) = parse_options(&argv) else {
        print_usage(PROGRAM_NAME);
        return ExitCode::FAILURE;
    };
    let options = set_options(options);

    if options.show_help {
        print_usage(PROGRAM_NAME);
        return ExitCode::SUCCESS;
    }

    if options.show_version {
        print_version(PROGRAM_NAME, PROGRAM_VERSION);
        print_version("sqlite", rusqlite::version());
        // wolfssl / yyjson / cwpack are not linked in the Rust port.
        return ExitCode::SUCCESS;
    }

    greeting();
    log_info!("{} {}", PROGRAM_NAME, PROGRAM_VERSION);
    log_info!("Port: {}", options.port);
    log_info!("Directory: {}", options.directory);
    log_info!("Threads: {}", options.threads);

    if options.directory != "." {
        if let Err(e) = mkdir_if_not_exists(&options.directory) {
            eprintln!("chdir: {}", e);
            return ExitCode::FAILURE;
        }
        if let Err(e) = std::env::set_current_dir(&options.directory) {
            eprintln!("chdir: {}", e);
            return ExitCode::FAILURE;
        }
    }

    // sqlite3_initialize(): sqlite initialises itself on first use now
    // (see .cargo/config.toml). wolfSSL_Init(): not linked.

    if database_create_if_not_exists() != 0 {
        return ExitCode::from(1);
    }

    let mut server = match TcpServer::create(options.port, 1024) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("Failed to create server: {}", e);
            return ExitCode::from(1);
        }
    };

    let mut pool = ThreadPool::start(options.threads, worker_handler);

    log_info!("Listening on port {}...", options.port);
    server.run(&mut SocketActions { pool: &pool });

    pool.stop();

    ExitCode::SUCCESS
}
