{
  description = "hjkl, a vim-modal terminal editor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Only the checks use it, to evaluate the home-manager module.
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      fenix,
      home-manager,
    }:
    let
      inherit (nixpkgs) lib;

      # No x86_64-darwin: nixpkgs-unstable dropped it and refuses to evaluate
      # for that platform.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      pkgsFor = lib.genAttrs systems (
        system:
        import nixpkgs {
          inherit system;
          overlays = [ self.overlays.default ];
        }
      );
      forAllSystems = f: lib.genAttrs systems (system: f pkgsFor.${system});

      toolchainFile = (lib.importTOML ./rust-toolchain.toml).toolchain;

      # rust-toolchain.toml names a channel rather than a release, and
      # fenix.fromToolchainFile can only fetch that with a hash which goes
      # stale on every Rust release. Fenix's own copy of the channel manifest
      # is pinned by its revision in flake.lock instead, so the toolchain is
      # reproducible and `nix flake update fenix` moves it forward. Cargo
      # refuses to build the workspace with a Rust older than its
      # rust-version, so a stale pin fails the package build loudly.
      rustChannel =
        system:
        fenix.packages.${system}.${toolchainFile.channel}
          or (throw "fenix has no Rust channel named `${toolchainFile.channel}`");

      craneLibWith =
        pkgs: pickToolchain:
        (crane.mkLib pkgs).overrideToolchain (p: pickToolchain (rustChannel p.stdenv.hostPlatform.system));
    in
    {
      overlays.default = final: _prev: {
        hjkl = final.callPackage ./nix/package.nix {
          craneLib = craneLibWith final (rust: rust.minimalToolchain);
        };
      };

      packages = forAllSystems (pkgs: {
        inherit (pkgs) hjkl;
        default = pkgs.hjkl;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = lib.getExe pkgs.hjkl;
          meta = { inherit (pkgs.hjkl.meta) description; };
        };
      });

      devShells = forAllSystems (pkgs: {
        default =
          (craneLibWith pkgs (
            rust:
            rust.withComponents (
              [
                "cargo"
                "rustc"
                "rust-std"
                "rust-src"
                "rust-analyzer"
              ]
              ++ toolchainFile.components
            )
          )).devShell
            {
              inputsFrom = [ pkgs.hjkl ];
              packages = with pkgs; [
                cargo-machete
                cargo-nextest
                git
                prettier
              ];
            };
      });

      nixosModules.default = import ./nix/nixos-module.nix self;
      homeManagerModules.default = import ./nix/home-manager-module.nix self;

      checks = forAllSystems (
        pkgs:
        import ./nix/checks.nix {
          inherit
            pkgs
            self
            nixpkgs
            home-manager
            ;
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
