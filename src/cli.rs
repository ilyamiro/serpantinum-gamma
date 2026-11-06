//! The command line client. It starts and stops the daemon and changes it over D-Bus, so callers
//! need no D-Bus tool of their own.

use std::fmt::Display;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::str::FromStr;
use std::thread::sleep;
use std::time::{Duration, Instant};

use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, Proxy};
use zbus::names::BusName;
use zbus::proxy::CacheProperties;

use crate::color::NEUTRAL_TEMP;
use crate::dbus::{BUS_NAME, INTERFACE, SCHEDULE_INTERFACE};
use crate::model::object_path;

pub const USAGE: &str = "\
Usage: serpantinum-gamma [COMMAND]

Without a command, runs the daemon in the foreground. It serves org.serpantinum.Gamma on the
session bus, and the commands below use it.

Commands:
  start                     Start the daemon in the background unless it is running
  quit                      Stop the daemon; the compositor restores normal colours
  status [-o OUTPUT]        Show the colour and the schedule; fails if the daemon is not running
  set [-o OUTPUT] [--temperature K] [--brightness 0-1] [--gamma G] [--inverted on|off]
                            Change the colour; a temperature turns the schedule off
  reset [-o OUTPUT]         Neutral temperature, full brightness, gamma 1, not inverted
  auto on|off [-o OUTPUT] [--day K] [--night K]
                            Follow the day and night schedule, or stop following it
  location LAT LON          Follow the sun at this place, in degrees
  location clear            Use the fallback times instead
  fallback DAY NIGHT        Times the fades to day and to night start, as HH:MM

Without -o (--output), a command changes every output, including outputs connected later.

Options:
  -h, --help     Show this help
  -V, --version  Show the version";

/// How long `start` waits for the daemon, and a command for an output that was just connected
const WAIT: Duration = Duration::from_secs(2);
const QUIT_WAIT: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(20);

enum Error {
    Usage(String),
    Failed(String),
    /// Fails with nothing more to say, as `status` does when the daemon is stopped
    Quiet,
}

type Result<T> = std::result::Result<T, Error>;

fn usage(message: impl Display) -> Error {
    Error::Usage(message.to_string())
}

fn failed(message: impl Display) -> Error {
    Error::Failed(message.to_string())
}

impl From<zbus::Error> for Error {
    fn from(error: zbus::Error) -> Self {
        failed(error)
    }
}

impl From<zbus::fdo::Error> for Error {
    fn from(error: zbus::fdo::Error) -> Self {
        failed(error)
    }
}

pub fn run(args: &[String]) -> ExitCode {
    match command(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Error::Usage(message)) => {
            eprintln!("serpantinum-gamma: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Error::Failed(message)) => {
            eprintln!("serpantinum-gamma: {message}");
            ExitCode::FAILURE
        }
        Err(Error::Quiet) => ExitCode::FAILURE,
    }
}

/// A command, the options it takes besides its words, and what it does. A test checks the options
/// against [`USAGE`].
struct Subcommand {
    name: &'static str,
    options: &'static [&'static str],
    run: fn(&Parsed) -> Result<()>,
}

const COMMANDS: &[Subcommand] = &[
    Subcommand { name: "start", options: &[], run: start },
    Subcommand { name: "quit", options: &[], run: quit },
    Subcommand { name: "status", options: &["--output"], run: status },
    Subcommand {
        name: "set",
        options: &["--output", "--temperature", "--brightness", "--gamma", "--inverted"],
        run: set,
    },
    Subcommand { name: "reset", options: &["--output"], run: reset },
    Subcommand { name: "auto", options: &["--output", "--day", "--night"], run: auto },
    Subcommand { name: "location", options: &[], run: location },
    Subcommand { name: "fallback", options: &[], run: fallback },
];

fn command(args: &[String]) -> Result<()> {
    let Some((name, rest)) = args.split_first() else { return Err(usage("no command")) };
    let Some(subcommand) = COMMANDS.iter().find(|subcommand| subcommand.name == name) else {
        return Err(usage(format!("unknown command '{name}'")));
    };
    (subcommand.run)(&parse(rest, subcommand.options)?)
}

/// A command's words and `--name value` options
struct Parsed<'a> {
    words: Vec<&'a str>,
    options: Vec<(&'a str, &'a str)>,
}

/// `-o` is short for `--output`. Words may start with a single `-`, as negative coordinates do.
fn parse<'a>(args: &'a [String], allowed: &[&str]) -> Result<Parsed<'a>> {
    let mut parsed = Parsed { words: Vec::new(), options: Vec::new() };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let name = if arg == "-o" { "--output" } else { arg.as_str() };
        if !name.starts_with("--") {
            parsed.words.push(arg);
            continue;
        }
        if !allowed.contains(&name) {
            return Err(usage(format!("unknown option '{arg}'")));
        }
        let value = args.next().ok_or_else(|| usage(format!("{arg} needs a value")))?;
        parsed.options.push((name, value));
    }
    Ok(parsed)
}

impl<'a> Parsed<'a> {
    fn words<const N: usize>(&self) -> Result<[&'a str; N]> {
        self.words.as_slice().try_into().map_err(|_| match self.words.get(N) {
            Some(extra) => usage(format!("unexpected '{extra}'")),
            None => usage("missing an argument"),
        })
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.options.iter().rev().find(|(option, _)| *option == name).map(|&(_, value)| value)
    }

    fn value<T: FromStr>(&self, name: &str) -> Result<Option<T>> {
        self.get(name).map(|value| number(value, name)).transpose()
    }

    fn switch(&self, name: &str) -> Result<Option<bool>> {
        self.get(name).map(|value| on_off(value, name)).transpose()
    }
}

fn number<T: FromStr>(value: &str, what: &str) -> Result<T> {
    value.parse().map_err(|_| usage(format!("'{value}' is not a valid value for {what}")))
}

fn on_off(value: &str, what: &str) -> Result<bool> {
    match value {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => Err(usage(format!("{what} takes on or off, not '{value}'"))),
    }
}

/// Minutes after midnight for `HH:MM`
fn clock_minutes(value: &str) -> Result<u16> {
    let parts = value.split_once(':').and_then(|(h, m)| Some((h.parse::<u16>().ok()?, m.parse::<u16>().ok()?)));
    match parts {
        Some((hours, minutes)) if hours < 24 && minutes < 60 && value.len() <= 5 => Ok(hours * 60 + minutes),
        _ => Err(usage(format!("'{value}' is not a time as HH:MM"))),
    }
}

fn connect() -> Result<Connection> {
    Connection::session().map_err(|e| failed(format!("cannot connect to the session bus: {e}")))
}

fn bus_name() -> BusName<'static> {
    BusName::try_from(BUS_NAME).expect("a valid bus name")
}

fn running(conn: &Connection) -> Result<bool> {
    Ok(DBusProxy::new(conn)?.name_has_owner(bus_name())?)
}

/// A connection to the running daemon. Commands never start it through D-Bus activation, whose
/// environment may lack WAYLAND_DISPLAY.
fn daemon() -> Result<Connection> {
    let conn = connect()?;
    if running(&conn)? {
        Ok(conn)
    } else {
        Err(failed("the daemon is not running; start it with serpantinum-gamma start"))
    }
}

fn proxy<'a>(conn: &Connection, path: &'a str, interface: &'static str) -> Result<Proxy<'a>> {
    Ok(zbus::blocking::proxy::Builder::new(conn)
        .destination(BUS_NAME)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()?)
}

/// The object for `output`, or `/` without one. A newly connected output may take a moment to
/// appear, so it is waited for.
fn target(conn: &Connection, output: Option<&str>) -> Result<String> {
    let Some(output) = output else { return Ok("/".to_owned()) };
    let path = object_path(output);
    let deadline = Instant::now() + WAIT;
    loop {
        let found = proxy(conn, &path, INTERFACE)?.get_property::<u16>("Temperature");
        match found {
            Ok(_) => return Ok(path),
            Err(_) if Instant::now() < deadline => sleep(POLL),
            Err(_) => return Err(failed(format!("output {output} is not connected"))),
        }
    }
}

fn start(parsed: &Parsed) -> Result<()> {
    parsed.words::<0>()?;
    let conn = connect()?;
    if running(&conn)? {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| failed(format!("cannot find this program: {e}")))?;
    let mut daemon = Command::new(exe);
    daemon.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe, so it may run between fork and exec. A session of its own
    // keeps the daemon alive when the caller's terminal or process group goes away.
    unsafe {
        daemon.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = daemon.spawn().map_err(|e| failed(format!("cannot start the daemon: {e}")))?;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if running(&conn)? {
            return Ok(());
        }
        // A daemon that lost a race to another one exits successfully, and the winner is waited for
        if let Ok(Some(status)) = child.try_wait()
            && !status.success()
        {
            return Err(failed("the daemon could not start; run serpantinum-gamma in a terminal to see why"));
        }
        sleep(POLL);
    }
    Err(failed("the daemon did not start in time"))
}

fn quit(parsed: &Parsed) -> Result<()> {
    parsed.words::<0>()?;
    let conn = connect()?;
    let Ok(pid) = DBusProxy::new(&conn)?.get_connection_unix_process_id(bus_name()) else { return Ok(()) };
    let pid = pid as libc::pid_t;
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        // SAFETY: kill only sends a signal
        unsafe { libc::kill(pid, signal) };
        let deadline = Instant::now() + QUIT_WAIT;
        while Instant::now() < deadline {
            if !running(&conn)? {
                return Ok(());
            }
            sleep(POLL);
        }
    }
    Err(failed(format!("the daemon (process {pid}) did not stop")))
}

fn status(parsed: &Parsed) -> Result<()> {
    parsed.words::<0>()?;
    let conn = connect()?;
    if !running(&conn)? {
        println!("stopped");
        return Err(Error::Quiet);
    }
    let path = target(&conn, parsed.get("--output"))?;
    let color = proxy(&conn, &path, INTERFACE)?;
    let schedule = proxy(&conn, &path, SCHEDULE_INTERFACE)?;
    let clock = |minutes: u16| format!("{:02}:{:02}", minutes / 60, minutes % 60);

    println!("running");
    println!("temperature {}", color.get_property::<u16>("Temperature")?);
    println!("brightness {}", color.get_property::<f64>("Brightness")?);
    println!("gamma {}", color.get_property::<f64>("Gamma")?);
    println!("inverted {}", color.get_property::<bool>("Inverted")?);
    println!("blocked {}", color.get_property::<bool>("Blocked")?);
    println!("auto {}", schedule.get_property::<bool>("Auto")?);
    println!("day-temperature {}", schedule.get_property::<u16>("DayTemperature")?);
    println!("night-temperature {}", schedule.get_property::<u16>("NightTemperature")?);
    if schedule.get_property::<bool>("HasLocation")? {
        let latitude = schedule.get_property::<f64>("Latitude")?;
        let longitude = schedule.get_property::<f64>("Longitude")?;
        println!("location {latitude} {longitude}");
        println!("sun-elevation {:.2}", schedule.get_property::<f64>("SunElevation")?);
    } else {
        println!("location none");
    }
    let day = clock(schedule.get_property::<u16>("FallbackDayStart")?);
    let night = clock(schedule.get_property::<u16>("FallbackNightStart")?);
    println!("fallback {day} {night}");
    Ok(())
}

fn set(parsed: &Parsed) -> Result<()> {
    parsed.words::<0>()?;
    let temperature = parsed.value::<u16>("--temperature")?;
    let brightness = parsed.value::<f64>("--brightness")?;
    let gamma = parsed.value::<f64>("--gamma")?;
    let inverted = parsed.switch("--inverted")?;
    if (temperature, brightness, gamma, inverted) == (None, None, None, None) {
        return Err(usage("set needs --temperature, --brightness, --gamma or --inverted"));
    }

    let conn = daemon()?;
    let path = target(&conn, parsed.get("--output"))?;
    let color = proxy(&conn, &path, INTERFACE)?;
    if let Some(value) = temperature {
        color.set_property("Temperature", value)?;
    }
    if let Some(value) = brightness {
        color.set_property("Brightness", value)?;
    }
    if let Some(value) = gamma {
        color.set_property("Gamma", value)?;
    }
    if let Some(value) = inverted {
        color.set_property("Inverted", value)?;
    }
    Ok(())
}

fn reset(parsed: &Parsed) -> Result<()> {
    parsed.words::<0>()?;
    let conn = daemon()?;
    let path = target(&conn, parsed.get("--output"))?;
    let color = proxy(&conn, &path, INTERFACE)?;
    color.set_property("Temperature", NEUTRAL_TEMP)?;
    color.set_property("Brightness", 1.0)?;
    color.set_property("Gamma", 1.0)?;
    color.set_property("Inverted", false)?;
    Ok(())
}

fn auto(parsed: &Parsed) -> Result<()> {
    let [state] = parsed.words::<1>()?;
    let state = on_off(state, "auto")?;
    let day = parsed.value::<u16>("--day")?;
    let night = parsed.value::<u16>("--night")?;

    let conn = daemon()?;
    let path = target(&conn, parsed.get("--output"))?;
    let schedule = proxy(&conn, &path, SCHEDULE_INTERFACE)?;
    if let Some(value) = day {
        schedule.set_property("DayTemperature", value)?;
    }
    if let Some(value) = night {
        schedule.set_property("NightTemperature", value)?;
    }
    schedule.set_property("Auto", state)?;
    Ok(())
}

fn location(parsed: &Parsed) -> Result<()> {
    let place = match parsed.words.as_slice() {
        ["clear"] => None,
        [latitude, longitude] => Some((number::<f64>(latitude, "latitude")?, number::<f64>(longitude, "longitude")?)),
        _ => return Err(usage("location takes LAT LON, or clear")),
    };
    let conn = daemon()?;
    let schedule = proxy(&conn, "/", SCHEDULE_INTERFACE)?;
    match place {
        Some(place) => schedule.call_method("SetLocation", &place)?,
        None => schedule.call_method("ClearLocation", &())?,
    };
    Ok(())
}

fn fallback(parsed: &Parsed) -> Result<()> {
    let [day, night] = parsed.words::<2>()?;
    let (day, night) = (clock_minutes(day)?, clock_minutes(night)?);
    let conn = daemon()?;
    let schedule = proxy(&conn, "/", SCHEDULE_INTERFACE)?;
    schedule.set_property("FallbackDayStart", day)?;
    schedule.set_property("FallbackNightStart", night)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn options_take_values_and_words_may_be_negative() {
        let raw = args(&["-o", "eDP-1", "--temperature", "4000", "-33.9", "--temperature", "3000"]);
        let parsed = parse(&raw, &["--output", "--temperature"]).ok().unwrap();
        assert_eq!(parsed.get("--output"), Some("eDP-1"));
        assert_eq!(parsed.value::<u16>("--temperature").ok().unwrap(), Some(3000));
        assert_eq!(parsed.words, ["-33.9"]);
    }

    #[test]
    fn unknown_or_incomplete_options_are_usage_errors() {
        assert!(matches!(parse(&args(&["--brightness", "1"]), &["--output"]), Err(Error::Usage(_))));
        assert!(matches!(parse(&args(&["--output"]), &["--output"]), Err(Error::Usage(_))));
        let raw = args(&["--temperature", "warm"]);
        let parsed = parse(&raw, &["--temperature"]).ok().unwrap();
        assert!(matches!(parsed.value::<u16>("--temperature"), Err(Error::Usage(_))));
        assert!(matches!(command(&args(&["frobnicate"])), Err(Error::Usage(_))));
        assert!(matches!(command(&args(&["set"])), Err(Error::Usage(_))));
        assert!(matches!(command(&args(&["auto", "maybe"])), Err(Error::Usage(_))));
        assert!(matches!(command(&args(&["location", "1"])), Err(Error::Usage(_))));
    }

    #[test]
    fn word_counts_are_checked() {
        let raw = args(&["a", "b"]);
        let parsed = parse(&raw, &[]).ok().unwrap();
        assert!(parsed.words::<2>().is_ok());
        assert!(matches!(parsed.words::<1>(), Err(Error::Usage(_))));
        assert!(matches!(parsed.words::<3>(), Err(Error::Usage(_))));
    }

    #[test]
    fn clock_times_parse_as_minutes() {
        assert_eq!(clock_minutes("06:30").ok(), Some(390));
        assert_eq!(clock_minutes("0:00").ok(), Some(0));
        assert_eq!(clock_minutes("23:59").ok(), Some(1439));
        assert!(clock_minutes("24:00").is_err());
        assert!(clock_minutes("7").is_err());
        assert!(clock_minutes("07:60").is_err());
        assert!(clock_minutes("07:000").is_err());
    }

    #[test]
    fn the_help_lists_the_options_each_command_takes() {
        for command in COMMANDS {
            // The command's lines: the one that starts with its name and those indented further
            let mut lines = USAGE.lines().skip_while(|line| line.split_whitespace().next() != Some(command.name));
            let mut text = lines.next().unwrap().to_owned();
            for line in lines.take_while(|line| line.starts_with("   ")) {
                text.push_str(line);
            }
            assert_eq!(text.contains("-o OUTPUT"), command.options.contains(&"--output"), "{}", command.name);
            let mut documented: Vec<_> = text
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .filter(|word| word.starts_with("--"))
                .collect();
            let mut taken: Vec<_> = command.options.iter().copied().filter(|&option| option != "--output").collect();
            documented.sort();
            taken.sort();
            assert_eq!(documented, taken, "{}", command.name);
        }
    }

    #[test]
    fn switches_read_on_and_off() {
        assert!(on_off("on", "x").ok().unwrap());
        assert!(!on_off("false", "x").ok().unwrap());
        assert!(on_off("sometimes", "x").is_err());
    }
}
