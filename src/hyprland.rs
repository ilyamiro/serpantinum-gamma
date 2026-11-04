//! Bindings for hyprland-ctm-control-v1, Hyprland's own colour control. Hyprland does not give
//! gamma ramps to clients, so it is driven through a colour matrix instead.

#![allow(non_upper_case_globals, non_camel_case_types, unused_imports, clippy::all)]

use wayland_client;
use wayland_client::protocol::*;

pub mod __interfaces {
    use wayland_client::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("protocols/hyprland-ctm-control-v1.xml");
}
use self::__interfaces::*;

wayland_scanner::generate_client_code!("protocols/hyprland-ctm-control-v1.xml");
