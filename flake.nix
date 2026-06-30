{
  description = "fleetix — typed fleet topology with Pkl schema, Rust bindings, and Nix modules";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rs-harbor = {
      url = "git+https://codeberg.org/caniko/rs-harbor.git";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.flake-utils.follows = "flake-utils";
      inputs.rust-overlay.follows = "rust-overlay";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      flake-utils,
      rust-overlay,
      rs-harbor,
      ...
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forSystems = nixpkgs.lib.genAttrs supportedSystems;

      pkgsFor = system: import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

      # Nix library: fromPkl + accessors
      fleetixLib = import ./lib { inherit (nixpkgs) lib; };
    in
    {
      lib = fleetixLib;

      # NixOS module for consuming fleetix topology from the sidecar
      nixosModules.topology = import ./modules/nixos/topology.nix { fleetixLib = self.lib; };
      homeModules.topology = import ./modules/home-manager/topology.nix { fleetixLib = self.lib; };

      packages = forSystems (system:
        let
          pkgs = pkgsFor system;
          craneLib = crane.mkLib pkgs;

          # Build the fleetix Rust crate
          fleetixCrate = craneLib.buildPackage {
            pname = "fleetix";
            version = "0.1.0";
            src = craneLib.cleanCargoSource ./.;
            strictDeps = true;
            doCheck = true;
            cargoExtraArgs = "--features cli";
          };

          # Flake app: evaluate a .pkl topology and produce a Nix expression
          evalPkl = pkgs.writeShellApplication {
            name = "fleetix-eval-pkl";
            runtimeInputs = [ fleetixCrate ];
            text = ''
              fleetix eval "$@"
            '';
          };

          # Export a .pkl topology to a Nix expression sidecar
          exportNix = pkgs.writeShellApplication {
            name = "fleetix-export-nix";
            runtimeInputs = [ fleetixCrate ];
            text = ''
              if [ $# -lt 1 ]; then
                echo "Usage: fleetix-export-nix <input.pkl> [output.nix]"
                exit 1
              fi
              input="$1"
              output="''${2:-}"
              if [ -z "$output" ]; then
                output="$(dirname "$input")/.fleetix-topology.nix"
              fi
              fleetix eval "$input" > "$output"
              echo "Wrote $output"
            '';
          };
        in
        {
          default = fleetixCrate;
          inherit fleetixCrate evalPkl exportNix;
        }
      );

      apps = forSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.fleetixCrate}/bin/fleetix";
        };
        eval-pkl = {
          type = "app";
          program = "${self.packages.${system}.evalPkl}/bin/fleetix-eval-pkl";
        };
        export-nix = {
          type = "app";
          program = "${self.packages.${system}.exportNix}/bin/fleetix-export-nix";
        };
      });

      checks = forSystems (system:
        let
          pkgs = pkgsFor system;
          craneLib = crane.mkLib pkgs;
          src = craneLib.cleanCargoSource ./.;
          commonArgs = {
            inherit src;
            pname = "fleetix";
            strictDeps = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        {
          fleetix-tests = craneLib.cargoTest (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--all-features";
            }
          );

          fleetix-clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--all-features";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );

          fleetix-fmt = craneLib.cargoFmt { inherit src; pname = "fleetix"; };

          validate-example-export = pkgs.runCommand "validate-example-export" {
            nativeBuildInputs = [ self.packages.${system}.fleetixCrate ];
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          } ''
            fleetix eval ${./examples/Topology.pkl} > topology.nix
            grep -q "hosts =" topology.nix
            grep -q "codebergPagesSites =" topology.nix
            grep -q "internalServices =" topology.nix
            grep -q "emailIdentities =" topology.nix
            touch $out
          '';
        }
      );

      devShells = forSystems (system:
        let
          pkgs = pkgsFor system;
          toolchain = rs-harbor.lib.mkToolchain { inherit pkgs; };
          cargoConfig = rs-harbor.lib.mkCargoConfig { inherit pkgs; };
          cross = rs-harbor.lib.mkCross { inherit pkgs system; };
        in
        (rs-harbor.lib.mkDevShells {
          inherit pkgs cross cargoConfig;
          inherit (toolchain) craneLib;
        })
      );
    };
}
