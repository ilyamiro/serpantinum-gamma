# serpantinum-gamma built from this checkout
{ pkgs, lib ? pkgs.lib }:

pkgs.rustPlatform.buildRustPackage {
  pname = "serpantinum-gamma";
  version = (lib.importTOML ../Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../assets ../LICENSE ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  postInstall = ''
    install -Dm644 assets/rs.wl-gammarelay.service $out/share/dbus-1/services/rs.wl-gammarelay.service
    substituteInPlace $out/share/dbus-1/services/rs.wl-gammarelay.service --replace-fail @out@ $out
  '';

  meta = {
    description = "Display temperature, brightness and gamma over D-Bus on Wayland, a drop-in for wl-gammarelay-rs";
    homepage = "https://github.com/ilyamiro/serpantinum-gamma";
    license = lib.licenses.agpl3Plus;
    mainProgram = "serpantinum-gamma";
    platforms = lib.platforms.linux;
  };
}
