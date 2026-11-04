# serpantinum-gamma from the binary of the latest GitHub release, which installs in seconds.
# release.json names that release; the release workflow updates it on master after each one.
{ pkgs, lib ? pkgs.lib }:

let
  release = lib.importJSON ./release.json;
in
pkgs.stdenv.mkDerivation {
  pname = "serpantinum-gamma";
  inherit (release) version;

  src = pkgs.fetchurl {
    url = "https://github.com/ilyamiro/serpantinum-gamma/releases/download/v${release.version}/serpantinum-gamma-${release.version}-x86_64-linux.tar.gz";
    inherit (release) hash;
  };

  # The tarball holds usr/ with the binary, the D-Bus service file and the licence
  sourceRoot = "usr";

  # The binary links only glibc and libgcc_s; autoPatchelf points it at Nix's
  nativeBuildInputs = [ pkgs.autoPatchelfHook ];
  buildInputs = [ pkgs.stdenv.cc.cc.lib ];

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp -r bin share $out/
    substituteInPlace $out/share/dbus-1/services/org.serpantinum.Gamma.service --replace-fail /usr/bin $out/bin
    runHook postInstall
  '';

  meta = {
    description = "Display temperature, brightness and gamma over D-Bus on Wayland";
    homepage = "https://github.com/ilyamiro/serpantinum-gamma";
    license = lib.licenses.agpl3Plus;
    mainProgram = "serpantinum-gamma";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
