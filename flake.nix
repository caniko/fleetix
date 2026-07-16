{
  description = "fleetix — typed fleet topology with Pkl schema, Rust bindings, and Nix modules";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rs-harbor = {
      url = "git+https://codeberg.org/caniko/rs-harbor.git?ref=trunk";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.rust-overlay.follows = "rust-overlay";
    };
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    rust-overlay,
    rs-harbor,
    ...
  }: let
    supportedSystems = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    forSystems = nixpkgs.lib.genAttrs supportedSystems;

    pkgsFor = system:
      import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

    # Nix library: fromPkl + accessors
    fleetixLib = import ./lib {inherit (nixpkgs) lib;};
  in {
    lib = fleetixLib;

    formatter = forSystems (system: (pkgsFor system).alejandra);

    # NixOS module for consuming fleetix topology from the sidecar
    nixosModules.topology = import ./modules/nixos.nix {fleetixLib = self.lib;};
    homeModules.topology = import ./modules/home-manager.nix;

    packages = forSystems (
      system: let
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
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        };

        # Flake app: evaluate a .pkl topology and produce a Nix expression
        evalPkl = pkgs.writeShellApplication {
          name = "fleetix-eval-pkl";
          runtimeInputs = [fleetixCrate];
          text = ''
            fleetix eval "$@"
          '';
        };

        # Export any Pkl file to an importable Nix expression sidecar.
        pklToNix = pkgs.writeShellApplication {
          name = "fleetix-pkl-to-nix";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.jq
            pkgs.pkl
          ];
          text = ''
            if [ "$#" -eq 1 ] && { [ "$1" = "--help" ] || [ "$1" = "-h" ]; }; then
              echo "Usage: fleetix-pkl-to-nix <input.pkl> <output.nix>"
              exit 0
            fi

            if [ $# -ne 2 ]; then
              echo "Usage: fleetix-pkl-to-nix <input.pkl> <output.nix>" >&2
              exit 1
            fi

            input="$1"
            output="$2"
            output_dir="$(dirname -- "$output")"
            mkdir -p -- "$output_dir"
            json_tmp="$(mktemp)"
            nix_tmp="$(mktemp "$output_dir/.fleetix-pkl-to-nix.XXXXXX")"
            trap 'rm -f -- "$json_tmp" "$nix_tmp"' EXIT

            pkl eval -f json "$input" > "$json_tmp"

            {
              echo "# Generated from $input; do not edit by hand."
              printf 'builtins.fromJSON '
              jq -Rs . < "$json_tmp"
            } > "$nix_tmp"
            if [ -e "$output" ]; then
              chmod --reference="$output" "$nix_tmp"
            fi
            mv -- "$nix_tmp" "$output"
            nix_tmp=""
            echo "Wrote $output from $input"
          '';
        };

        # Export a .pkl topology to a Nix expression sidecar
        exportNix = pkgs.writeShellApplication {
          name = "fleetix-export-nix";
          runtimeInputs = [fleetixCrate];
          text = ''
            if [ "$#" -eq 1 ] && { [ "$1" = "--help" ] || [ "$1" = "-h" ]; }; then
              echo "Usage: fleetix-export-nix <input.pkl> [output.nix]"
              exit 0
            fi

            if [ $# -lt 1 ] || [ $# -gt 2 ]; then
              echo "Usage: fleetix-export-nix <input.pkl> [output.nix]" >&2
              exit 1
            fi
            input="$1"
            output="''${2:-}"
            if [ -z "$output" ]; then
              output="$(dirname "$input")/.fleetix-topology.nix"
            fi
            fleetix export "$input" "$output"
            echo "Wrote $output"
          '';
        };
      in {
        default = fleetixCrate;
        inherit fleetixCrate evalPkl exportNix pklToNix;
      }
    );

    apps = forSystems (system: {
      default = {
        type = "app";
        program = "${self.packages.${system}.fleetixCrate}/bin/fleetix";
        meta.description = "Evaluate and export Fleetix Pkl topology";
      };
      eval-pkl = {
        type = "app";
        program = "${self.packages.${system}.evalPkl}/bin/fleetix-eval-pkl";
        meta.description = "Evaluate a Pkl topology through Fleetix";
      };
      export-nix = {
        type = "app";
        program = "${self.packages.${system}.exportNix}/bin/fleetix-export-nix";
        meta.description = "Export a Pkl topology as a Nix sidecar";
      };
      pkl-to-nix = {
        type = "app";
        program = "${self.packages.${system}.pklToNix}/bin/fleetix-pkl-to-nix";
        meta.description = "Convert Pkl JSON output to an importable Nix expression";
      };
    });

    checks = forSystems (
      system: let
        pkgs = pkgsFor system;
        craneLib = crane.mkLib pkgs;
        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          pname = "fleetix";
          strictDeps = true;
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      in {
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

        fleetix-no-default-features = craneLib.cargoClippy (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--no-default-features";
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          }
        );

        fleetix-publication-policy = assert (cargoToml.package.publish or true) == false;
          pkgs.runCommand "fleetix-publication-policy" {} ''
            touch $out
          '';

        fleetix-fmt = craneLib.cargoFmt {
          inherit src;
          pname = "fleetix";
        };

        validate-example-export =
          pkgs.runCommand "validate-example-export" {
            nativeBuildInputs = [self.packages.${system}.fleetixCrate];
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          } ''
            fleetix eval ${./examples/Topology.pkl} > topology.nix
            grep -q "hosts =" topology.nix
            grep -q "codebergPagesSites =" topology.nix
            grep -q "redirects =" topology.nix
            grep -q "internalServices =" topology.nix
            grep -q "emailIdentities =" topology.nix
            grep -q "buildCache =" topology.nix
            grep -q "packageAttrNames =" topology.nix
            grep -q '"dashboard-api"' topology.nix
            grep -q 'keyPrefix = "edge-a"' topology.nix
            touch $out
          '';

        validate-modular-aggregate-export =
          pkgs.runCommand "validate-modular-aggregate-export" {
            nativeBuildInputs = [self.packages.${system}.pklToNix];
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          } ''
            fixture="$TMPDIR/topology"
            mkdir -p "$fixture/links" "$fixture/hosts"

            cat > "$fixture/Schema.pkl" <<'EOF'
            class Link {
              subnet: String
            }

            class Host {
              system: String
            }
            EOF

            cat > "$fixture/links/WgHome.pkl" <<'EOF'
            import "../Schema.pkl" as S

            links = new {
              ["wg-home"] = new S.Link {
                subnet = "10.123.0.0/24"
              }
            }
            EOF

            cat > "$fixture/hosts/Atlas.pkl" <<'EOF'
            import "../Schema.pkl" as S

            hosts = new {
              ["atlas"] = new S.Host {
                system = "x86_64-linux"
              }
            }
            EOF

            cat > "$fixture/Domains.pkl" <<'EOF'
            domains = new {
              zones = new Listing<String> {
                "example.test"
              }
              redirects = new Listing {
                new {
                  from = "old.example.test"
                  to = "new.example.test"
                  status = 301
                  preservePath = true
                }
              }
            }
            EOF

            cat > "$fixture/Services.pkl" <<'EOF'
            services = new {
              reverseProxyServices = new Listing {}
            }
            EOF

            cat > "$fixture/Topology.aggregated.pkl" <<'EOF'
            links = new {
              ["wg-home"] = (import("links/WgHome.pkl")).links["wg-home"]
            }

            hosts = new {
              atlas = (import("hosts/Atlas.pkl")).hosts["atlas"]
            }

            domains = (import("Domains.pkl")).domains
            services = (import("Services.pkl")).services
            EOF

            fleetix-pkl-to-nix "$fixture/Topology.aggregated.pkl" topology.nix
            grep -Fq '\"links\":' topology.nix
            grep -Fq '\"hosts\":' topology.nix
            grep -Fq '\"domains\":' topology.nix
            grep -Fq '\"services\":' topology.nix
            grep -q "wg-home" topology.nix
            grep -Fq '\"redirects\":' topology.nix
            touch $out
          '';

        fleetix-lib-helpers = let
          topology = {
            links = {
              wg-home = {
                subnet = "10.123.0.0/24";
                port = 54321;
                endpointSubdomain = "wg";
              };
              direct-link.subnet = "10.10.0.0/24";
            };
            hosts = {
              atlas = {
                network = {
                  lanIp = "192.168.178.88";
                  directLinkIp = "10.10.0.1";
                };
                links.wg-home = {
                  address = "10.123.0.5";
                  role = "server";
                  publicKey = "server-key";
                };
              };
              nomad = {
                network = {};
                links.direct-link.address = "10.10.0.2";
              };
            };
            domains = {
              zones = [
                "example.test"
                "internal.example.test"
              ];
              managedZones = [
                "example.test"
                "internal.example.test"
              ];
              dynamicHosts = [
                {
                  fqdn = "example.test";
                  proxied = true;
                }
                {
                  fqdn = "wg.example.test";
                  proxied = false;
                }
                {
                  fqdn = "host.internal.example.test";
                  proxied = false;
                }
              ];
              codebergPagesSites = [
                {
                  subdomain = "docs";
                  targetRepo = "example/docs";
                }
              ];
            };
            services.reverseProxyServices = [
              {
                name = "immich";
                hostname = "immich.example.test";
                port = 2283;
                targetHost = "atlas";
                lanExposed = true;
                cloudflareProxied = true;
              }
              {
                name = "ollama";
                hostname = "ollama.internal.example.test";
                port = 11434;
                targetHost = "atlas";
                vpnOnly = true;
              }
            ];
            services.staticFileServices = [
              {
                name = "docs";
                hostname = "docs.example.test";
                cloudflareProxied = false;
              }
            ];
          };
          endpoint = self.lib.services.serviceEndpoint {
            inherit topology;
            serviceName = "immich";
            addressPolicy = ["direct-link" "lan"];
          };
          missingEndpoint = self.lib.services.serviceEndpoint {
            inherit topology;
            serviceName = "missing";
            addressPolicy = ["lan"];
            require = false;
          };
          nodes = self.lib.adapters.infernix.mkFleetNodes {
            inherit topology;
            nodes = {
              atlas = {
                address = null;
                modelPort = null;
                models.qwen3-vl = {
                  name = "qwen3-vl";
                  capabilities = ["chat"];
                };
              };
              nomad = {
                addressPolicy = ["direct-link"];
                modelPort = 8015;
                models.embed = {
                  name = "embed";
                  capabilities = ["embeddings"];
                };
              };
            };
          };
          addressExcludes = self.lib.domains.dynamicHostAddressExcludes {
            inherit topology;
            zone = "example.test";
          };
          internalAddressExcludes = self.lib.domains.dynamicHostAddressExcludes {
            inherit topology;
            zone = "internal.example.test";
          };
          serviceIntents = self.lib.services.serviceCnameIntents {inherit topology;};
          pagesIntents = self.lib.services.codebergPagesCnameIntents {inherit topology;};
          normalized = self.lib.projections.normalize {inherit topology;};
        in
          pkgs.runCommand "fleetix-lib-helpers" {} ''
            test "${self.lib.hosts.resolveHostAddress {
              inherit topology;
              hostName = "atlas";
              policy = ["lan"];
            }}" = "192.168.178.88"
            test "${self.lib.hosts.resolveHostAddress {
              inherit topology;
              hostName = "nomad";
              policy = ["direct-link"];
            }}" = "10.10.0.2"
            test "${endpoint.url}" = "http://10.10.0.1:2283"
            test "${
              if missingEndpoint == null
              then "null"
              else "unexpected"
            }" = "null"
            test "${self.lib.domains.zoneForHost {
              inherit topology;
              fqdn = "host.internal.example.test";
            }}" = "internal.example.test"
            test "${self.lib.domains.relativeName {
              fqdn = "wg.example.test";
              zone = "example.test";
            }}" = "wg"
            test "${toString (builtins.length addressExcludes)}" = "4"
            test "${(builtins.elemAt addressExcludes 0).name}" = "@"
            test "${(builtins.elemAt addressExcludes 2).name}" = "wg"
            test "${toString (builtins.length internalAddressExcludes)}" = "2"
            test "${(builtins.elemAt internalAddressExcludes 0).name}" = "host"
            test "${(self.lib.services.serviceHosts {inherit topology;}).immich}" = "immich.example.test"
            test "${toString (builtins.length serviceIntents)}" = "2"
            test "${(builtins.elemAt serviceIntents 0).relativeName}" = "immich"
            test "${(builtins.elemAt serviceIntents 0).target}" = "example.test"
            test "${toString (builtins.elemAt serviceIntents 0).proxied}" = "1"
            test "${(builtins.elemAt pagesIntents 0).relativeName}" = "docs"
            test "${(builtins.elemAt pagesIntents 0).target}" = "docs.example.codeberg.page"
            test "${normalized.hosts.atlas.network.wgHomeIp}" = "10.123.0.5"
            test "${normalized.domains.serviceHosts.immich}" = "immich.example.test"
            test "${normalized.links.wg-home.serverAddress}" = "10.123.0.5"
            test "${normalized.services.reverseProxyByName.immich.targetHost}" = "atlas"
            test "${toString (self.lib.firewall.lanExposedPorts {
              inherit topology;
              hostName = "atlas";
            })}" = "2283"
            test "${nodes.atlas.address}" = "192.168.178.88"
            test "${toString nodes.atlas.modelPort}" = "8013"
            test "${nodes.nomad.address}" = "10.10.0.2"
            test "${toString nodes.nomad.modelPort}" = "8015"
            touch $out
          '';

        module-integration-fixtures = let
          nixos = nixpkgs.lib.evalModules {
            modules = [self.nixosModules.topology];
          };
          integratedHome = nixpkgs.lib.evalModules {
            modules = [
              self.homeModules.topology
              {_module.args.osConfig = nixos.config;}
            ];
          };
          standaloneHome = nixpkgs.lib.evalModules {
            modules = [
              self.homeModules.topology
              {_module.args.osConfig = null;}
            ];
          };
        in
          pkgs.runCommand "fleetix-module-integration-fixtures" {} ''
            test "${toString (builtins.hasAttr "fleetix" nixos.config)}" = 1
            test "${toString (builtins.length (builtins.attrNames integratedHome.config.fleetix.topology))}" = 0
            test "${toString (builtins.length (builtins.attrNames standaloneHome.config.fleetix.topology))}" = 0
            touch $out
          '';
      }
    );

    devShells = forSystems (
      system: let
        pkgs = pkgsFor system;
        toolchain = rs-harbor.lib.mkToolchain {inherit pkgs;};
        cargoConfig = rs-harbor.lib.mkCargoConfig {inherit pkgs;};
        cross = rs-harbor.lib.mkCross {inherit pkgs system;};
        stableToolchain = pkgs.rust-bin.stable."1.96.1".default;
        stableCargoConfig = rs-harbor.lib.mkCargoConfig {
          inherit pkgs;
          channel = "stable";
        };
        stableCross = rs-harbor.lib.mkCross {
          inherit pkgs system;
          enableOsxcross = false;
        };
        msrvToolchain = pkgs.rust-bin.stable."1.88.0".default;
        msrvCargoConfig = rs-harbor.lib.mkCargoConfig {
          inherit pkgs;
          channel = "stable";
        };
        msrvCross = rs-harbor.lib.mkCross {
          inherit pkgs system;
          enableOsxcross = false;
        };
      in
        (rs-harbor.lib.mkDevShells {
          inherit pkgs cross cargoConfig;
          inherit (toolchain) craneLib;
        })
        // {
          stable = rs-harbor.lib.mkDevShell {
            inherit pkgs;
            craneLib = (crane.mkLib pkgs).overrideToolchain (_: stableToolchain);
            cargoConfig = stableCargoConfig;
            cross = stableCross;
            packages = [stableToolchain];
            enableWindowsEnv = false;
            enableOsxcrossEnv = false;
          };
          msrv = rs-harbor.lib.mkDevShell {
            inherit pkgs;
            craneLib = (crane.mkLib pkgs).overrideToolchain (_: msrvToolchain);
            cargoConfig = msrvCargoConfig;
            cross = msrvCross;
            packages = [msrvToolchain];
            enableWindowsEnv = false;
            enableOsxcrossEnv = false;
          };
        }
    );
  };
}
