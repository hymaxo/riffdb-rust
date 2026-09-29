use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub const LOG_H_BUFSIZE: usize = 4096;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum LogVerbosity {
    None = 0,
    Trace = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl LogVerbosity {
    fn from_u8(v: u8) -> LogVerbosity {
        match v {
            1 => LogVerbosity::Trace,
            2 => LogVerbosity::Info,
            3 => LogVerbosity::Warn,
            4 => LogVerbosity::Error,
            5 => LogVerbosity::Fatal,
            _ => LogVerbosity::None,
        }
    }
}

static LOG_MAX_VERBOSITY: AtomicU8 = AtomicU8::new(LogVerbosity::Trace as u8);
static LOG_COLORED: AtomicBool = AtomicBool::new(true);
const LOG_ADD_NEW_LINE: bool = true;
const LOG_ADD_DATE: bool = false;

#[cfg_attr(debug_assertions, allow(dead_code))]
pub fn set_max_verbosity(v: LogVerbosity) {
    LOG_MAX_VERBOSITY.store(v as u8, Ordering::Relaxed);
}

#[inline(always)]
pub fn enabled(verbosity: LogVerbosity) -> bool {
    let max = LOG_MAX_VERBOSITY.load(Ordering::Relaxed);
    max != LogVerbosity::None as u8 && verbosity as u8 >= max
}

pub fn set_colored(colored: bool) {
    LOG_COLORED.store(colored, Ordering::Relaxed);
}

static LOG_VERBOSITY_STRINGS: [&str; 6] = [
    "why you're watching inside binary?",
    "[TRACE]",
    "[INFO] ",
    "[WARN] ",
    "[ERROR]",
    "[FATAL]",
];

static LOG_VERBOSITY_STRINGS_COLORED: [&str; 6] = [
    "sfome on the swag",
    "[\x1b[0;34mTRACE\x1b[1;0m]",
    "[\x1b[0;32mINFO\x1b[1;0m] ",
    "[\x1b[0;33mWARN\x1b[1;0m] ",
    "[\x1b[0;31mERROR\x1b[1;0m]",
    "[\x1b[0;31mFATAL\x1b[1;0m]",
];

#[cold]
#[inline(never)]
pub fn log_flog(
    verbosity: LogVerbosity,
    stream: Stream,
    line: u32,
    filename: &str,
    args: fmt::Arguments,
) {
    let max_verbosity = LogVerbosity::from_u8(LOG_MAX_VERBOSITY.load(Ordering::Relaxed));
    if max_verbosity == LogVerbosity::None || verbosity == LogVerbosity::None {
        return;
    }
    if verbosity < max_verbosity {
        return;
    }

    let local_log_colored = LOG_COLORED.load(Ordering::Relaxed);

    let now = chrono::Local::now();
    let time_buffer = if LOG_ADD_DATE {
        now.format("%Y-%m-%d %H:%M:%S")
    } else {
        now.format("%H:%M:%S")
    };

    let mut string_buffer = String::with_capacity(LOG_H_BUFSIZE + 256);
    let _ = fmt::write(
        &mut string_buffer,
        format_args!(
            "{} {} {}:{}: ",
            time_buffer,
            if local_log_colored {
                LOG_VERBOSITY_STRINGS_COLORED[verbosity as usize]
            } else {
                LOG_VERBOSITY_STRINGS[verbosity as usize]
            },
            filename,
            line
        ),
    );

    let prefix_len = string_buffer.len();
    let _ = fmt::write(&mut string_buffer, args);

    let max = prefix_len + LOG_H_BUFSIZE - 2;
    if string_buffer.len() > max {
        let mut cut = max;
        while !string_buffer.is_char_boundary(cut) {
            cut -= 1;
        }
        string_buffer.truncate(cut);
    }

    if LOG_ADD_NEW_LINE {
        string_buffer.push('\n');
    }

    match stream {
        Stream::Stdout => {
            let _ = std::io::stdout().lock().write_all(string_buffer.as_bytes());
        }
        Stream::Stderr => {
            let _ = std::io::stderr().lock().write_all(string_buffer.as_bytes());
        }
    }
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::LogVerbosity::Trace) {
            $crate::log::log_flog($crate::log::LogVerbosity::Trace, $crate::log::Stream::Stderr, line!(), file!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::LogVerbosity::Info) {
            $crate::log::log_flog($crate::log::LogVerbosity::Info, $crate::log::Stream::Stdout, line!(), file!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::LogVerbosity::Warn) {
            $crate::log::log_flog($crate::log::LogVerbosity::Warn, $crate::log::Stream::Stderr, line!(), file!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! log_err {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::LogVerbosity::Error) {
            $crate::log::log_flog($crate::log::LogVerbosity::Error, $crate::log::Stream::Stderr, line!(), file!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! log_fatal {
    ($($arg:tt)*) => {{
        $crate::log::log_flog($crate::log::LogVerbosity::Fatal, $crate::log::Stream::Stderr, line!(), file!(), format_args!($($arg)*));
        ::std::process::abort();
    }};
}
