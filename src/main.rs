// Port of main.h / main.c
//
// Direct, unsafe-heavy port of https://github.com/ssleert/riffdb.
// Module layout is 1:1 with the C sources; see README.md for the mapping and
// the list of deliberate deviations (marked `PORT FIX` / `PORT NOTE` in code).

#![allow(clippy::missing_safety_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

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
mod sys;
mod tcp_server;
mod thread_pool;
mod utils;
mod worker;
mod xmalloc;

use libc::c_void;
use libsqlite3_sys::{sqlite3_initialize, sqlite3_libversion};
use std::ffi::CStr;
use std::process::ExitCode;

use crate::database::database_create_if_not_exists;
use crate::greeting::greeting;
use crate::options::{parse_options, print_usage, print_version, set_options};
use crate::socket_actions::{socket_actions_on_connect, socket_actions_on_disconnect, socket_actions_on_readable};
use crate::tcp_server::{tcp_server_create, tcp_server_destroy, tcp_server_run, tcp_server_set_callbacks, TcpServer};
use crate::thread_pool::{thread_pool_start, thread_pool_stop, ThreadPool};
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
        // SAFETY: sqlite3_libversion returns a static NUL-terminated string.
        let sqlite_version = unsafe { CStr::from_ptr(sqlite3_libversion()) };
        print_version("sqlite", &sqlite_version.to_string_lossy());
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

    unsafe { run_server(options.port, options.threads) }
}

unsafe fn run_server(port: u16, threads: u8) -> ExitCode {
    if sqlite3_initialize() != 0 {
        log_err!("cant init sqlite");
        return ExitCode::from(1);
    }

    // Replaces wolfSSL_Init(): the only platform init we need is WinSock.
    if sys::init() != 0 {
        log_err!("cant init sockets");
        return ExitCode::from(1);
    }

    if database_create_if_not_exists() != 0 {
        return ExitCode::from(1);
    }

    let mut server = TcpServer::zeroed();
    let server_ptr: *mut TcpServer = &mut server;

    if tcp_server_create(server_ptr, port, 1024) != 0 {
        eprintln!("Failed to create server: {}", std::io::Error::last_os_error());
        return ExitCode::from(1);
    }

    // Workers keep a raw pointer to the pool, so it must not move. It lives
    // on this stack frame for the whole program, exactly like the C version.
    // All accesses go through the one raw pointer so none of them invalidate
    // the pointers the workers hold.
    let mut pool = ThreadPool::new();
    let pool_ptr: *mut ThreadPool = &mut pool;
    if thread_pool_start(pool_ptr, threads, worker_handler) != 0 {
        log_err!("Failed to start thread pool");
        return ExitCode::from(1);
    }

    tcp_server_set_callbacks(
        server_ptr,
        socket_actions_on_connect,
        socket_actions_on_readable,
        socket_actions_on_disconnect,
        pool_ptr as *mut c_void,
    );

    log_info!("Listening on port {}...", port);
    tcp_server_run(server_ptr);

    thread_pool_stop(pool_ptr);

    tcp_server_destroy(server_ptr);

    ExitCode::SUCCESS
}
