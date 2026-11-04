use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsFd;

use calloop::ping::Ping;
use rustix::fs::{MemfdFlags, memfd_create};
use wayland_client::globals::{GlobalList, GlobalListContents};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1;
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_v1::{self, ZwlrGammaControlV1};

use crate::color::{Color, ramp_bytes};
use crate::dbus::{self, Relay, Shared};
use crate::model::{Target, object_path};

// wl_output's name event arrived in version 4
const OUTPUT_VERSION: u32 = 4;

struct Slot {
    id: u32,
    output: WlOutput,
    name: Option<String>,
    control: Option<ZwlrGammaControlV1>,
    size: u32,
    applied: Option<Color>,
    failed: bool,
}

/// The Wayland side: one gamma control per output, kept in line with the model
pub struct Daemon {
    model: Shared,
    bus: zbus::blocking::Connection,
    wake: Ping,
    manager: ZwlrGammaControlManagerV1,
    qh: QueueHandle<Daemon>,
    slots: Vec<Slot>,
    /// False until the bus name is ours: a gamma control is exclusive, so none is taken before
    active: bool,
}

impl Daemon {
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Daemon>,
        model: Shared,
        bus: zbus::blocking::Connection,
        wake: Ping,
    ) -> Result<Daemon, String> {
        let manager = globals
            .bind(qh, 1..=1, ())
            .map_err(|_| "the compositor does not support wlr-gamma-control-unstable-v1".to_owned())?;
        let mut daemon = Daemon { model, bus, wake, manager, qh: qh.clone(), slots: Vec::new(), active: false };
        let registry = globals.registry();
        globals.contents().with_list(|list| {
            for global in list.iter().filter(|global| global.interface == WlOutput::interface().name) {
                daemon.bind_output(registry, global.name, global.version);
            }
        });
        Ok(daemon)
    }

    pub fn activate(&mut self) {
        self.active = true;
        self.reconcile();
    }

    /// Applies the schedule for the current time and sends what changed
    pub fn tick(&mut self) {
        dbus::tick(&self.model, &self.bus);
        self.reconcile();
    }

    /// A D-Bus change came in. Outputs that refused a control get another try.
    pub fn woken(&mut self) {
        for slot in &mut self.slots {
            slot.failed = false;
        }
        self.reconcile();
    }

    fn bind_output(&mut self, registry: &WlRegistry, id: u32, version: u32) {
        if version < OUTPUT_VERSION {
            eprintln!("serpantinum-gamma: skipping an output whose wl_output version {version} has no name");
            return;
        }
        let output = registry.bind::<WlOutput, _, _>(id, OUTPUT_VERSION, &self.qh, id);
        self.slots.push(Slot { id, output, name: None, control: None, size: 0, applied: None, failed: false });
    }

    fn output_named(&mut self, id: u32, name: String) {
        let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == id) else { return };
        if slot.name.is_some() {
            return;
        }
        slot.name = Some(name.clone());
        let path = object_path(&name);
        self.model.lock().unwrap().add_output(id, name);
        let relay = Relay::new(
            self.model.clone(),
            Target::Output(id),
            path.clone(),
            self.bus.inner().clone(),
            self.wake.clone(),
        );
        if let Err(error) = self.bus.object_server().at(path.as_str(), relay) {
            eprintln!("serpantinum-gamma: could not serve {path}: {error}");
        }
        self.reconcile();
    }

    fn remove_output(&mut self, id: u32) {
        let Some(position) = self.slots.iter().position(|slot| slot.id == id) else { return };
        let slot = self.slots.remove(position);
        if let Some(control) = slot.control {
            control.destroy();
        }
        slot.output.release();
        if let Some(name) = slot.name {
            self.model.lock().unwrap().outputs.remove(&id);
            let path = object_path(&name);
            if let Err(error) = self.bus.object_server().remove::<Relay, _>(path.as_str()) {
                eprintln!("serpantinum-gamma: could not stop serving {path}: {error}");
            }
        }
    }

    fn reconcile(&mut self) {
        if !self.active {
            return;
        }
        let model = self.model.lock().unwrap();
        for slot in &mut self.slots {
            let Some(color) = model.outputs.get(&slot.id).map(|output| output.color) else { continue };
            if slot.failed {
                continue;
            }
            let Some(control) = &slot.control else {
                // The ramp is sent once the compositor answers with the ramp size
                slot.control = Some(self.manager.get_gamma_control(&slot.output, &self.qh, slot.id));
                continue;
            };
            if slot.size == 0 || slot.applied == Some(color) {
                continue;
            }
            match write_ramp(control, slot.size as usize, color) {
                Ok(()) => slot.applied = Some(color),
                Err(error) => eprintln!("serpantinum-gamma: could not send a gamma ramp: {error}"),
            }
        }
    }
}

fn write_ramp(control: &ZwlrGammaControlV1, size: usize, color: Color) -> std::io::Result<()> {
    let fd = memfd_create("serpantinum-gamma", MemfdFlags::CLOEXEC)?;
    let mut file = File::from(fd);
    file.write_all(&ramp_bytes(size, color))?;
    file.seek(SeekFrom::Start(0))?;
    control.set_gamma(file.as_fd());
    Ok(())
}

impl Dispatch<WlRegistry, GlobalListContents> for Daemon {
    fn event(
        daemon: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } if interface == WlOutput::interface().name => {
                daemon.bind_output(registry, name, version);
            }
            wl_registry::Event::GlobalRemove { name } => daemon.remove_output(name),
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for Daemon {
    fn event(
        daemon: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            daemon.output_named(*id, name);
        }
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for Daemon {
    fn event(
        _: &mut Self,
        _: &ZwlrGammaControlManagerV1,
        _: <ZwlrGammaControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrGammaControlV1, u32> for Daemon {
    fn event(
        daemon: &mut Self,
        _: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(slot) = daemon.slots.iter_mut().find(|slot| slot.id == *id) else { return };
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => {
                slot.size = size;
                slot.applied = None;
                daemon.reconcile();
            }
            zwlr_gamma_control_v1::Event::Failed => {
                let name = slot.name.as_deref().unwrap_or("an output");
                eprintln!(
                    "serpantinum-gamma: the compositor refused gamma control of {name}; another program may hold it"
                );
                slot.failed = true;
                slot.size = 0;
                slot.applied = None;
                if let Some(control) = slot.control.take() {
                    control.destroy();
                }
            }
            _ => {}
        }
    }
}
