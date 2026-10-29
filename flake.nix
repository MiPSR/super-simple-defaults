{
  description = "Native default-applications selector (Rust/iced, Wayland)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      libs = [ pkgs.wayland pkgs.libxkbcommon pkgs.vulkan-loader pkgs.fontconfig pkgs.freetype pkgs.expat pkgs.libGL pkgs.libx11 pkgs.libxcursor pkgs.libxrandr pkgs.libxi ];
      libPath = pkgs.lib.makeLibraryPath libs;
    in {
      devShells.${system}.default = pkgs.mkShell {
        buildInputs = [ pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy pkgs.gcc pkgs.pkg-config pkgs.desktop-file-utils ] ++ libs;
        shellHook = ''
          export LD_LIBRARY_PATH="${libPath}:$LD_LIBRARY_PATH"
        '';
      };

      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "super-simple-defaults";
        version = "0.1.0";
        src = pkgs.lib.cleanSourceWith {
          src = pkgs.lib.cleanSource ./.;
          filter = path: type: baseNameOf path != "target";
        };
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper pkgs.desktop-file-utils ];
        buildInputs = libs;
        doCheck = true;
        postInstall = ''
          install -Dm644 assets/io.github.MiPSR.SuperSimpleDefaults.desktop \
            $out/share/applications/io.github.MiPSR.SuperSimpleDefaults.desktop
          install -Dm644 assets/super-simple-defaults.svg \
            $out/share/icons/hicolor/scalable/apps/io.github.MiPSR.SuperSimpleDefaults.svg
          desktop-file-validate $out/share/applications/io.github.MiPSR.SuperSimpleDefaults.desktop
          ln -s ssd $out/bin/super-simple-defaults
        '';
        postFixup = ''
          wrapProgram $out/bin/ssd --prefix LD_LIBRARY_PATH : "${libPath}"
        '';
        meta = {
          description = "Native default-application editor";
          mainProgram = "ssd";
          platforms = [ system ];
        };
      };

      checks.${system}.default = self.packages.${system}.default;
    };
}
