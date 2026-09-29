use std::sync::OnceLock;

const DEFAULT_PORT: u16 = 9889;

#[derive(Debug, Clone)]
pub struct Options {
    pub port: u16,
    pub directory: String,
    pub threads: u8,
    pub show_help: bool,
    pub show_version: bool,
}

static OPTIONS: OnceLock<Options> = OnceLock::new();

pub fn options() -> &'static Options {
    OPTIONS.get().expect("options not initialised")
}

pub fn set_options(options: Options) -> &'static Options {
    let _ = OPTIONS.set(options);
    self::options()
}

fn default_threads() -> u8 {
    std::thread::available_parallelism()
        .map(|n| n.get().min(u8::MAX as usize) as u8)
        .unwrap_or(1)
}

pub fn print_usage(program_name: &str) {
    print!(
        "Usage: {} [OPTIONS]\n\
         \n\
         Options:\n\
         \x20 -p, --port PORT          Listen port (default: {})\n\
         \x20 -d, --directory DIR      Working directory (default: current directory)\n\
         \x20 -t, --threads N          Number of threads (default: number of logical CPU cores)\n\
         \x20 -h, --help               Show this help message and exit\n\
         \x20 -v, --version            Show version information and exit\n\
         \n",
        program_name, DEFAULT_PORT
    );
}

pub fn print_version(program_name: &str, version: &str) {
    println!("{} {}", program_name, version);
}

struct LongOption {
    name: &'static str,
    has_arg: bool,
    val: char,
}

static LONOPTIONS: [LongOption; 5] = [
    LongOption { name: "port", has_arg: true, val: 'p' },
    LongOption { name: "directory", has_arg: true, val: 'd' },
    LongOption { name: "threads", has_arg: true, val: 't' },
    LongOption { name: "help", has_arg: false, val: 'h' },
    LongOption { name: "version", has_arg: false, val: 'v' },
];

const SHORT_OPTIONS: &str = "p:d:t:hv";

fn short_has_arg(c: char) -> Option<bool> {
    if c == ':' {
        return None;
    }
    let pos = SHORT_OPTIONS.find(c)?;
    Some(SHORT_OPTIONS.as_bytes().get(pos + 1) == Some(&b':'))
}

fn parse_ranged(s: &str, min: i64, max: i64) -> Option<i64> {
    let t = s.trim_start();
    if t.is_empty() {
        return None;
    }
    let v: i64 = t.parse().ok()?;
    if v < min || v > max {
        return None;
    }
    Some(v)
}

fn handle_opt(opts: &mut Options, opt: char, optarg: Option<&str>, argv0: &str) -> Result<(), ()> {
    match opt {
        'p' => {
            let a = optarg.unwrap_or("");
            match parse_ranged(a, 1, 65535) {
                Some(v) => opts.port = v as u16,
                None => {
                    eprintln!("{}: invalid port number '{}'", argv0, a);
                    return Err(());
                }
            }
        }
        'd' => opts.directory = optarg.unwrap_or("").to_string(),
        't' => {
            let a = optarg.unwrap_or("");
            match parse_ranged(a, 1, 255) {
                Some(v) => opts.threads = v as u8,
                None => {
                    eprintln!("{}: invalid thread count '{}'", argv0, a);
                    return Err(());
                }
            }
        }
        'h' => opts.show_help = true,
        'v' => opts.show_version = true,
        _ => return Err(()),
    }
    Ok(())
}

pub fn parse_options(argv: &[String]) -> Result<Options, ()> {
    let mut opts = Options {
        port: DEFAULT_PORT,
        directory: ".".to_string(),
        threads: default_threads(),
        show_help: false,
        show_version: false,
    };

    let argv0 = argv.first().map(String::as_str).unwrap_or("riffdb");
    let mut non_options: Vec<&str> = Vec::new();

    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;

        if arg == "--" {
            non_options.extend(argv[i..].iter().map(String::as_str));
            break;
        }

        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline_val) = match long.find('=') {
                Some(eq) => (&long[..eq], Some(&long[eq + 1..])),
                None => (long, None),
            };

            let exact = LONOPTIONS.iter().find(|o| o.name == name);
            let matched = match exact {
                Some(o) => Some(o),
                None => {
                    let candidates: Vec<&LongOption> =
                        LONOPTIONS.iter().filter(|o| o.name.starts_with(name)).collect();
                    if candidates.len() > 1 {
                        eprintln!("{}: option '--{}' is ambiguous", argv0, name);
                        return Err(());
                    }
                    candidates.first().copied()
                }
            };

            let Some(o) = matched else {
                eprintln!("{}: unrecognized option '--{}'", argv0, name);
                return Err(());
            };

            let optarg: Option<&str> = if o.has_arg {
                match inline_val {
                    Some(v) => Some(v),
                    None if i < argv.len() => {
                        i += 1;
                        Some(argv[i - 1].as_str())
                    }
                    None => {
                        eprintln!("{}: option '--{}' requires an argument", argv0, o.name);
                        return Err(());
                    }
                }
            } else {
                if inline_val.is_some() {
                    eprintln!("{}: option '--{}' doesn't allow an argument", argv0, o.name);
                    return Err(());
                }
                None
            };

            handle_opt(&mut opts, o.val, optarg, argv0)?;
            continue;
        }

        if arg.len() > 1 && arg.starts_with('-') {
            let chars: Vec<(usize, char)> = arg.char_indices().skip(1).collect();
            let mut k = 0;
            while k < chars.len() {
                let (pos, c) = chars[k];
                k += 1;

                let Some(has_arg) = short_has_arg(c) else {
                    eprintln!("{}: invalid option -- '{}'", argv0, c);
                    return Err(());
                };

                if has_arg {
                    let rest = &arg[pos + c.len_utf8()..];
                    let optarg = if !rest.is_empty() {
                        rest
                    } else if i < argv.len() {
                        i += 1;
                        argv[i - 1].as_str()
                    } else {
                        eprintln!("{}: option requires an argument -- '{}'", argv0, c);
                        return Err(());
                    };
                    handle_opt(&mut opts, c, Some(optarg), argv0)?;
                    break;
                }

                handle_opt(&mut opts, c, None, argv0)?;
            }
            continue;
        }

        non_options.push(arg);
    }

    if !non_options.is_empty() {
        eprint!("{}: unexpected argument(s):", argv0);
        for a in non_options {
            eprint!(" {}", a);
        }
        eprintln!();
        return Err(());
    }

    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, ()> {
        let argv: Vec<String> = std::iter::once("riffdb").chain(args.iter().copied()).map(String::from).collect();
        parse_options(&argv)
    }

    #[test]
    fn defaults() {
        let o = parse(&[]).unwrap();
        assert_eq!(o.port, DEFAULT_PORT);
        assert_eq!(o.directory, ".");
        assert!(o.threads >= 1);
        assert!(!o.show_help && !o.show_version);
    }

    #[test]
    fn short_long_and_attached_forms() {
        let o = parse(&["-p", "80", "--directory=/tmp/x", "-t4", "-hv"]).unwrap();
        assert_eq!((o.port, o.directory.as_str(), o.threads), (80, "/tmp/x", 4));
        assert!(o.show_help && o.show_version);

        let o = parse(&["--port", "81", "--thr", "2"]).unwrap();
        assert_eq!((o.port, o.threads), (81, 2));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&["-p", "0"]).is_err());
        assert!(parse(&["-p", "65536"]).is_err());
        assert!(parse(&["-p", "12x"]).is_err());
        assert!(parse(&["-t", "256"]).is_err());
        assert!(parse(&["-p"]).is_err());
        assert!(parse(&["-x"]).is_err());
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--help=1"]).is_err());
        assert!(parse(&["stray"]).is_err());
        assert!(parse(&["--", "-p", "1"]).is_err());
    }
}
