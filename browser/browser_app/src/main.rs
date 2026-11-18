#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod document;
mod render;
mod site;
mod ui;

use ui::{chrome, window};

use std::{
    env, fmt,
    fs::File,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Clone)]
struct Log {
    output: Option<Arc<Mutex<File>>>,
    start: Instant,
}

impl Log {
    fn new(path: Option<PathBuf>) -> std::io::Result<Self> {
        Ok(Self {
            output: path
                .map(File::create)
                .transpose()?
                .map(|file| Arc::new(Mutex::new(file))),
            start: Instant::now(),
        })
    }

    fn write(&self, message: fmt::Arguments<'_>) {
        if let Some(output) = &self.output {
            let _ = writeln!(
                output.lock().unwrap(),
                "{:>9.2} ms {message}",
                self.start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

struct Options {
    profile: PathBuf,
    log: Option<PathBuf>,
    file: Option<PathBuf>,
    url: Option<network_stack::Url>,
    warp: bool,
    motion: bool,
    motion_probe: Option<u32>,
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut profile = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(env::temp_dir)
            .join("Browser");
        let mut log = None;
        let mut file = None;
        let mut url = None;
        let mut warp = false;
        let mut motion = false;
        let mut motion_probe = None;
        let mut args = env::args_os().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--profile") => {
                    profile = args
                        .next()
                        .map(PathBuf::from)
                        .ok_or("--profile needs a directory.")?
                }
                Some("--log") => {
                    log = Some(
                        args.next()
                            .map(PathBuf::from)
                            .ok_or("--log needs a file path.")?,
                    )
                }
                Some("--warp") => warp = true,
                Some("--url") => {
                    let input = args
                        .next()
                        .and_then(|arg| arg.into_string().ok())
                        .ok_or("--url needs a domain.")?;
                    url = Some(network_stack::address(&input)?);
                }
                Some("--motion-demo") => motion = true,
                Some("--motion-probe") => {
                    let cycles = args
                        .next()
                        .and_then(|count| count.to_str()?.parse::<u32>().ok())
                        .filter(|count| (1..=100).contains(count))
                        .ok_or("--motion-probe needs a cycle count from 1 to 100.")?;
                    motion_probe = Some(cycles);
                    motion = true;
                }
                Some(flag) if flag.starts_with("--") => {
                    return Err(format!("Unknown option {flag}."));
                }
                _ if file.is_none() => file = Some(PathBuf::from(arg)),
                _ => return Err("Open one HTML file at a time.".into()),
            }
        }
        if file.is_some() && url.is_some() {
            return Err("Open one page at a time.".into());
        }
        if motion_probe.is_some() && (log.is_none() || file.is_some() || url.is_some()) {
            return Err("--motion-probe requires --log and opens the Motion Engine demo.".into());
        }
        Ok(Self {
            profile,
            log,
            file,
            url,
            warp,
            motion,
            motion_probe,
        })
    }
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    if let Some(mode) = arguments.next()
        && mode == "--sandbox-probe"
    {
        let Some(output) = arguments.next().map(PathBuf::from) else {
            std::process::exit(2)
        };
        let result = sandbox_probe(&output);
        let saved = std::fs::write(
            &output,
            match &result {
                Ok(report) | Err(report) => report,
            },
        );
        std::process::exit(i32::from(result.is_err() || saved.is_err()));
    }
    let mut interactive = true;
    let result = (|| {
        let mut options = Options::parse()?;
        interactive = options.motion_probe.is_none();
        let log = Log::new(options.log.take())
            .map_err(|error| format!("Could not open the log. {error}"))?;
        log.write(format_args!(
            "browser={} startup",
            env!("CARGO_PKG_VERSION")
        ));
        window::run(options, log)
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        if interactive {
            window::show_error(&error);
        }
        std::process::exit(1);
    }
}

fn sandbox_probe(output: &std::path::Path) -> Result<String, String> {
    use site::WebContentProcess;
    use std::net::TcpListener;
    use web_content_process::{Reply, Request};
    let file = output.with_extension(format!("{}.private", std::process::id()));
    File::options()
        .create_new(true)
        .write(true)
        .open(&file)
        .and_then(|mut file| file.write_all(b"Sandbox probe"))
        .map_err(|error| error.to_string())?;
    let report = (|| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
        let worker = WebContentProcess::start()?;
        let private_file = std::path::absolute(&file)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .into_owned();
        let loopback_port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let Reply::Probe(report) = worker.call(Request::Probe {
            private_file,
            loopback_port,
        })?
        else {
            return Err("Invalid sandbox probe reply.".into());
        };
        if report.file_blocked && report.network_blocked && report.child_blocked {
            Ok(format!("{report:?}"))
        } else {
            Err(format!("{report:?}"))
        }
    })();
    let _ = std::fs::remove_file(file);
    report
}
