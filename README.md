# serpantinum-gamma

A small daemon that sets display colour temperature, brightness, gamma and inversion on Wayland.
It uses the `wlr-gamma-control-unstable-v1` protocol, supported by Hyprland, Sway, niri, mango and
other wlroots-style compositors. GNOME and KDE don't support it.

It serves the same D-Bus interface as [wl-gammarelay-rs](https://github.com/MaxVerevkin/wl-gammarelay-rs):
the bus name `rs.wl-gammarelay`, the interface `rs.wl.gammarelay`, and the objects `/` and `/outputs/<name>`.
Scripts written for wl-gammarelay-rs work unchanged. The binary links only glibc.

## Usage

```sh
serpantinum-gamma &      # or let D-Bus start it, see below

busctl --user set-property rs.wl-gammarelay / rs.wl.gammarelay Temperature q 4000
busctl --user -- call rs.wl-gammarelay / rs.wl.gammarelay UpdateTemperature n -100
busctl --user set-property rs.wl-gammarelay /outputs/eDP_1 rs.wl.gammarelay Brightness d 0.8
busctl --user call rs.wl-gammarelay / rs.wl.gammarelay ToggleInverted
busctl --user tree rs.wl-gammarelay
```

| Member | Type | Range |
|---|---|---|
| `Temperature` | property `q` | 1000–10000 K, 6500 is neutral |
| `Brightness` | property `d` | 0–1 |
| `Gamma` | property `d` | at least 0.1, 1 is neutral |
| `Inverted` | property `b` | |
| `UpdateTemperature` | method `n` | adds a delta |
| `UpdateBrightness`, `UpdateGamma` | method `d` | adds a delta |
| `ToggleInverted` | method | |

Output names become path elements with every character other than a letter or digit replaced by `_`,
so `eDP-1` is `/outputs/eDP_1`. On `/`, the values read are the average of all outputs (`Inverted` is
true only if every output is inverted), and writes go to every output.

All properties emit `PropertiesChanged`, on the object that changed and on `/` when the average moves.

### Differences from wl-gammarelay-rs

- Values set on `/` also apply to outputs connected later, and are kept while no output is connected.
  wl-gammarelay-rs starts new outputs at 6500 K.
- The colour for each temperature comes from Tanner Helland's blackbody fit, so a given temperature
  looks slightly different than in wl-gammarelay-rs.
- There is no `watch` subcommand. Use `busctl --user monitor rs.wl-gammarelay` instead.

## Starting it through D-Bus

The package installs `share/dbus-1/services/rs.wl-gammarelay.service`, so the first call to
`rs.wl-gammarelay` starts the daemon. A process D-Bus starts gets D-Bus's own environment, so
`WAYLAND_DISPLAY` must be in it. Most compositor setups already run
`dbus-update-activation-environment --systemd WAYLAND_DISPLAY`. If yours doesn't, start the daemon
from the compositor's autostart instead.

Don't install this next to wl-gammarelay-rs: both claim the same bus name, and only one can run.

## Installing

Each [release](https://github.com/ilyamiro/serpantinum-gamma/releases) has an x86_64 tarball laid out
under `usr/`, plus a `SHA256SUMS` file:

```sh
tar -xzf serpantinum-gamma-*-x86_64-linux.tar.gz -C / --no-same-owner   # as root
```

On NixOS:

```nix
let gamma = pkgs.callPackage /path/to/serpantinum-gamma { }; in {
  environment.systemPackages = [ gamma ];
  services.dbus.packages = [ gamma ];
}
```

From source: `cargo build --release`, then `packaging/stage.sh <dest>` lays out the files.

## Releasing

Bump `version` in `Cargo.toml`, commit, and push a `v<version>` tag. The release workflow tests,
builds the tarball, publishes the release, and then commits the new hash to `nix/release.json`.

## Licence

GNU Affero General Public License version 3, or (at your option) any later version. See [LICENSE](LICENSE).
