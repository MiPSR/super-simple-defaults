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
        buildInputs = [ pkgs.rustc pkgs.cargo pkgs.gcc pkgs.pkg-config ] ++ libs;
        shellHook = ''
          export LD_LIBRARY_PATH="${libPath}:$LD_LIBRARY_PATH"
        '';
      };

      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "super-simple-defaults";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
        buildInputs = libs;
        postFixup = ''
          wrapProgram $out/bin/super-simple-defaults --prefix LD_LIBRARY_PATH : "${libPath}"
        '';
      };
    };
}