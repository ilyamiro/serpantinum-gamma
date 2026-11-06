# serpantinum-gamma

A small daemon that sets display colour temperature, brightness, gamma and inversion on Wayland,
with a day and night schedule that follows the sun. The same binary is its command line client. It uses the `wlr-gamma-control-unstable-v1` protocol, supported by
Sway, niri, mango and other wlroots-style compositors, and on Hyprland, which no longer gives
gamma ramps to clients, its own `hyprland-ctm-control-v1` colour matrix. The backend is picked on
its own: Hyprland's if the compositor offers it, otherwise wlroots'. GNOME and KDE support neither.
On Hyprland only temperature and brightness take effect; a matrix is linear, so gamma and
inversion are accepted but have no effect there.
The binary links only glibc.

## Usage

```sh
serpantinum-gamma start                                   # the daemon, in the background
serpantinum-gamma set --temperature 4000                  # every output
serpantinum-gamma set -o eDP-1 --brightness 0.8           # one output
serpantinum-gamma auto on --night 3500                    # follow the schedule
serpantinum-gamma location 55.75 37.62                    # by the sun at this place
serpantinum-gamma fallback 06:30 19:00                    # or, without a location, by the clock
serpantinum-gamma status -o eDP-1
serpantinum-gamma reset
serpantinum-gamma quit                                    # the compositor restores normal colours
```

`serpantinum-gamma --help` lists every command. Without `-o`, a command changes every output,
including outputs connected later. A command for an output that was just connected waits up to
two seconds for it to appear. Commands never start the daemon themselves, except `start`.

The daemon keeps its colours only while it runs, since the compositor restores normal colours
when a program's gamma control goes away. It can also run in the foreground, as plain
`serpantinum-gamma`.

## D-Bus

The client talks to the daemon over the session bus, which any other program can use too. No
systemd is needed, only a session bus.

```sh
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
| `Blocked` | read-only property `b` | true while the compositor ignores the colours, as when another program holds the control; not signalled |
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

## Day and night schedule

The `org.serpantinum.Gamma.Schedule` interface changes the temperature with the time of day. Like
the colour, it is on `/` for every output and on `/outputs/<name>` for one. The daemon keeps no
settings and finds no location itself: whatever starts it sets them, and they last until it exits.

```sh
busctl --user call org.serpantinum.Gamma / org.serpantinum.Gamma.Schedule SetLocation dd 55.75 37.62
busctl --user set-property org.serpantinum.Gamma / org.serpantinum.Gamma.Schedule NightTemperature q 3500
busctl --user set-property org.serpantinum.Gamma / org.serpantinum.Gamma.Schedule Auto b true
busctl --user set-property org.serpantinum.Gamma /outputs/HDMI_A_1 org.serpantinum.Gamma.Schedule Auto b false
```

| Member | Type | Meaning |
|---|---|---|
| `Auto` | property `b` | follow the schedule; off at start; on `/`, true only if every output follows it |
| `DayTemperature`, `NightTemperature` | property `q` | default 6500 and 4000 K |
| `SetLocation` | method `dd` | latitude −90..90 and longitude −180..180, in degrees |
| `ClearLocation` | method | back to the fallback times |
| `FallbackDayStart`, `FallbackNightStart` | property `q` | minutes after local midnight, default 420 (07:00) and 1140 (19:00) |
| `HasLocation`, `Latitude`, `Longitude` | read-only `b`, `d`, `d` | 0 without a location |
| `Sunrise`, `Sunset` | read-only `t` | today's, in Unix seconds; 0 without a location or on a day without one |
| `SunElevation` | read-only `d` | the sun's height in degrees now; 0 without a location |

- `Auto`, `DayTemperature` and `NightTemperature` belong to each output. Writing them on `/` sets
  them on every output, including outputs connected later, and reading them there gives the
  average. The location and the fallback times are shared: setting them on any object sets them for
  all, and every object shows the same values.
- With a location, the temperature follows the sun's elevation: day at +2° and above, night at
  −4.5° and below, fading in between. That takes about 30–40 minutes at mid latitudes. The sun's
  position comes from NOAA's equations and agrees with PyEphem to about 0.01°.
- Without a location, the fade to night starts at `FallbackNightStart` and the fade to day at
  `FallbackDayStart`, each lasting 30 minutes.
- The fade is even in mireds (a million over the temperature), the scale on which equal steps look
  equally large.
- The schedule is applied every 10 seconds from the current clock, so suspend, time zone and
  daylight saving changes take effect at the next tick. Places without a sunrise or sunset, as in
  polar night, simply stay at night or day.
- "Today" for `Sunrise` and `Sunset` is the system's local day.
- Setting `Temperature` or calling `UpdateTemperature` turns `Auto` off on the same object, so on
  every output for `/` and on that output alone for `/outputs/<name>`. Brightness,
  gamma and inversion are left alone by the schedule.
- All properties emit `PropertiesChanged`, including the read-only ones as time passes.

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
