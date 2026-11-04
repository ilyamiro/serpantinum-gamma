# serpantinum-gamma

A small daemon that sets display colour temperature, brightness, gamma and inversion on Wayland,
controlled over D-Bus. It uses the `wlr-gamma-control-unstable-v1` protocol, supported by Hyprland,
Sway, niri, mango and other wlroots-style compositors. GNOME and KDE don't support it.
The binary links only glibc.

## Usage

```sh
serpantinum-gamma &      # or let D-Bus start it, see below

busctl --user set-property org.serpantinum.Gamma / org.serpantinum.Gamma Temperature q 4000
busctl --user -- call org.serpantinum.Gamma / org.serpantinum.Gamma UpdateTemperature n -100
busctl --user set-property org.serpantinum.Gamma /outputs/eDP_1 org.serpantinum.Gamma Brightness d 0.8
busctl --user call org.serpantinum.Gamma / org.serpantinum.Gamma ToggleInverted
busctl --user tree org.serpantinum.Gamma
```

The bus name and the interface are both `org.serpantinum.Gamma`. `/` controls every output, and
`/outputs/<name>` controls one.

| Member | Type | Range |
|---|---|---|
| `Temperature` | property `q` | 1000–10000 K, 6500 is neutral |
| `Brightness` | property `d` | 0–1 |
| `Gamma` | property `d` | at least 0.1, 1 is neutral |
| `Inverted` | property `b` | |
| `UpdateTemperature` | method `n` | adds a delta |
| `UpdateBrightness`, `UpdateGamma` | method `d` | adds a delta |
| `ToggleInverted` | method | |

- Output names become path elements with every character other than a letter or digit replaced
  by `_`, so `eDP-1` is `/outputs/eDP_1`.
- Reading `/` gives the average of all outputs. `Inverted` is true only if every output is inverted.
- Writing to `/` changes every output, including outputs connected later. The value is kept even
  while no output is connected.
- `ToggleInverted` on `/` returns every output to normal if all are inverted, and otherwise inverts
  them all.
- All properties emit `PropertiesChanged`, on the object that changed and on `/` when the average moves.
  Use `busctl --user monitor org.serpantinum.Gamma` to watch them.

## Colour

The colour for each temperature is computed from physics, not a curve fit:

1. Planck's law gives the blackbody spectrum.
2. The CIE 1931 colour matching functions turn it into XYZ.
3. A Bradford chromatic adaptation maps the 6500 K blackbody onto the display's D65 white, so
   6500 K is exactly neutral and the colours change smoothly around it.
4. The sRGB transfer function encodes the result, because a gamma ramp scales encoded values.

## Starting it through D-Bus

The package installs `share/dbus-1/services/org.serpantinum.Gamma.service`, so the first call to
`org.serpantinum.Gamma` starts the daemon. A process D-Bus starts gets D-Bus's own environment, so
`WAYLAND_DISPLAY` must be in it. Most compositor setups already run
`dbus-update-activation-environment --systemd WAYLAND_DISPLAY`. If yours doesn't, start the daemon
from the compositor's autostart instead.

Gamma control is exclusive, so only one program can hold it per output. Stop any other night-light
tool first, or the compositor refuses the daemon's control and it logs a message.

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
