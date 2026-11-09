{
  # lD Mixer for Nix and NixOS.
  #
  #   nix run github:lovebycoma/ldmixer            # try it
  #   nix profile install github:lovebycoma/ldmixer
  #   ld-mixer uninstall                            # remove it again
  #
  # On NixOS, USB access needs the udev rule from the package; add the module:
  #   imports = [ ld-mixer.nixosModules.default ];
  #   programs.ld-mixer.enable = true;
  description = "lD Mixer: unofficial mixer for Audient iD interfaces";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "ld-mixer";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [ pkg-config wrapGAppsHook4 gnumake ];
          buildInputs = with pkgs; [ gtk4 libadwaita libusb1 ];
          cargoTestFlags = [ "--lib" ];
          # pw-loopback / pw-dump for the virtual devices.
          preFixup = ''
            gappsWrapperArgs+=(--prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.pipewire ]})
          '';
          # cargo already built the binaries; install them plus data files.
          installPhase = ''
            runHook preInstall
            make install TARGET=target/${pkgs.stdenv.hostPlatform.rust.cargoShortTarget}/release \
              DESTDIR= NO_UDEV_RELOAD=1 PREFIX=$out UDEVDIR=$out/lib/udev/rules.d
            runHook postInstall
          '';
          meta = with pkgs.lib; {
            description = "Unofficial mixer and monitor control for Audient iD interfaces";
            homepage = "https://github.com/lovebycoma/ldmixer";
            license = licenses.mit;
            platforms = platforms.linux;
            mainProgram = "ld-mixer";
          };
        };
      });

      nixosModules.default = { config, lib, pkgs, ... }: {
        options.programs.ld-mixer.enable = lib.mkEnableOption "lD Mixer";
        config = lib.mkIf config.programs.ld-mixer.enable {
          environment.systemPackages = [ self.packages.${pkgs.system}.default ];
          services.udev.packages = [ self.packages.${pkgs.system}.default ];
        };
      };

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.system}.default ];
          packages = with pkgs; [ cargo rustc clippy rustfmt ];
        };
      });
    };
}
