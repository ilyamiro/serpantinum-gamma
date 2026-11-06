mod cli;
mod clock;
mod color;
mod dbus;
mod hyprland;
mod model;
mod schedule;
mod solar;
mod wayland;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use calloop::EventLoop;
use calloop::ping::make_ping;
use calloop::timer::{TimeoutAction, Timer};
use calloop_wayland_source::WaylandSource;
use wayland_client::Connection;
use wayland_client::globals::registry_queue_init;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use crate::dbus::{BUS_NAME, Handle};
use crate::model::{Model, Target};
use crate::wayland::Daemon;

// How often the schedule is applied. The clock is read fresh each time, so this is also the longest
// the colour can lag behind after a resume from suspend or a change of the clock or time zone.
const TICK: Duration = Duration::from_secs(10);

enum Outcome {
    Served,
    AlreadyRunning,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("serpantinum-gamma {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(_) => return cli::run(&args),
    }
    match run() {
        Ok(Outcome::Served) => ExitCode::SUCCESS,
        Ok(Outcome::AlreadyRunning) => {
            eprintln!("serpantinum-gamma: {BUS_NAME} is already served on this bus");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("serpantinum-gamma: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Outcome, String> {
    let bus = zbus::blocking::Connection::session().map_err(|e| format!("cannot connect to the session bus: {e}"))?;
    let wayland = Connection::connect_to_env().map_err(|e| format!("cannot connect to the Wayland compositor: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<Daemon>(&wayland).map_err(|e| e.to_string())?;

    let model = Arc::new(Mutex::new(Model::default()));
    let (wake, wake_source) = make_ping().map_err(|e| e.to_string())?;
    let mut daemon = Daemon::new(&globals, &queue.handle(), model.clone(), bus.clone(), wake.clone())?;

    // Learn the outputs' names and serve every object before taking the name, so a client that sees
    // the name also finds /outputs/<name>
    queue.roundtrip(&mut daemon).map_err(|e| e.to_string())?;
    let root = Handle::new(model, Target::Root, "/".to_owned(), bus.inner().clone(), wake);
    dbus::serve(&bus, root).map_err(|e| e.to_string())?;

    match bus.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => {
            return Ok(Outcome::AlreadyRunning);
        }
        Err(error) => return Err(format!("cannot take the name {BUS_NAME}: {error}")),
    }
    daemon.activate();

    let mut event_loop: EventLoop<Daemon> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    WaylandSource::new(wayland, queue).insert(handle.clone()).map_err(|e| e.to_string())?;
    handle.insert_source(wake_source, |_, _, daemon| daemon.woken()).map_err(|e| e.to_string())?;
    // The timer runs on the monotonic clock, which stops during suspend, so after a resume it fires
    // within one tick
    let timer = Timer::from_duration(TICK);
    handle
        .insert_source(timer, |_, _, daemon| {
            daemon.tick();
            TimeoutAction::ToDuration(TICK)
        })
        .map_err(|e| e.to_string())?;

    // Runs until the compositor goes away, which also restores its gamma
    event_loop.run(None, &mut daemon, |_| {}).map_err(|e| format!("lost the Wayland connection: {e}"))?;
    Ok(Outcome::Served)
}
