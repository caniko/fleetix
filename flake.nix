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
      url = "git+https://codeberg.org/caniko/rs-harbor.git?ref=trunk&rev=c26b735eede8078f795651c4a9cbf0be8733b221";
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

    # NixOS and Home Manager modules for consuming fleetix topology from the
    # sidecar (Home Manager mirrors the active NixOS module when both are loaded)
    nixosModules.topology = import ./modules/topology.nix {fleetixLib = self.lib;};
    homeModules.topology = import ./modules/topology.nix {fleetixLib = self.lib;};
    homeModules.trust-observer = import ./modules/trust-observer.nix;

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

        # Export any Pkl file to an importable Nix expression sidecar.
        pklToNix = pkgs.writeShellApplication {
          name = "fleetix-pkl-to-nix";
          runtimeInputs = [fleetixCrate];
          text = ''
            if [ "$#" -eq 1 ] && { [ "$1" = "--help" ] || [ "$1" = "-h" ]; }; then
              echo "Usage: fleetix-pkl-to-nix <input.pkl> <output.nix>"
              exit 0
            fi

            if [ $# -ne 2 ]; then
              echo "Usage: fleetix-pkl-to-nix <input.pkl> <output.nix>" >&2
              exit 1
            fi

            exec fleetix pkl-to-nix "$@"
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
        inherit fleetixCrate exportNix pklToNix;
      }
    );

    apps = forSystems (system: {
      default = {
        type = "app";
        program = "${self.packages.${system}.fleetixCrate}/bin/fleetix";
        meta.description = "Evaluate and export Fleetix Pkl topology";
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
        assertionsModule = {
          options.assertions = nixpkgs.lib.mkOption {
            type = nixpkgs.lib.types.listOf nixpkgs.lib.types.attrs;
            default = [];
          };
        };
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
            fixture="$TMPDIR/example-root"
            mkdir -p "$fixture/examples" "$fixture/lib/topology"
            cp ${./examples/Topology.pkl} "$fixture/examples/Topology.pkl"
            cp ${./lib/topology/Schema.pkl} "$fixture/lib/topology/Schema.pkl"
            fleetix eval "$fixture/examples/Topology.pkl" > topology.nix
            grep -q "schemaVersion = 2" topology.nix
            grep -q "hosts =" topology.nix
            grep -q "pagesSites =" topology.nix
            grep -q "redirects =" topology.nix
            grep -q "endpoints =" topology.nix
            grep -q "httpSites =" topology.nix
            grep -q "buildCache =" topology.nix
            grep -q "packageAttrNames =" topology.nix
            grep -q 'dashboard' topology.nix
            grep -q 'requiredAvailability = "always-on"' topology.nix
            grep -q 'serviceIntents' topology.nix
            grep -q 'sshKnownHosts' topology.nix
            grep -q 'git.example.test' topology.nix
            touch $out
          '';

        validate-modular-aggregate-export =
          pkgs.runCommand "validate-modular-aggregate-export" {
            nativeBuildInputs = [self.packages.${system}.pklToNix];
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          } ''
            fixture="$TMPDIR/topology"
            mkdir -p "$fixture/links" "$fixture/hosts"
            mkdir -p "$TMPDIR/shared"
            : > "$TMPDIR/shared/Names.pkl"

            cat > "$fixture/Schema.pkl" <<'EOF'
            class Link {
              subnet: String
            }

            class Host {
              system: String
            }

            class SshKnownHost {
              hostNames: Listing<String>
              publicKeys: Listing<String>
            }

            class Trust {
              sshKnownHosts: Listing<SshKnownHost> = new Listing {}
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
              endpoints = new {}
              httpSites = new {}
            }
            EOF

            cat > "$fixture/Trust.pkl" <<'EOF'
            import "Schema.pkl" as S

            trust = new S.Trust {
              sshKnownHosts = new Listing<S.SshKnownHost> {
                new S.SshKnownHost {
                  hostNames = new Listing { "git.example.test" }
                  publicKeys = new Listing { "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleGit" }
                }
              }
            }
            EOF

            cat > "$fixture/Topology.aggregated.pkl" <<'EOF'
            links = new {
              ["wg-home"] = (import("links/WgHome.pkl")).links["wg-home"]
            }

            hosts = new {
              atlas = (import("hosts/Atlas.pkl")).hosts["atlas"]
            }

            schemaVersion: UInt16 = 2
            domains = (import("Domains.pkl")).domains
            services = (import("Services.pkl")).services
            trust = (import("Trust.pkl")).trust
            EOF

            fleetix-pkl-to-nix "$fixture/Topology.aggregated.pkl" topology.nix
            grep -Fq 'links = {' topology.nix
            grep -Fq 'hosts = {' topology.nix
            grep -Fq 'domains = {' topology.nix
            grep -Fq 'services = {' topology.nix
            grep -q 'wg-home =' topology.nix
            grep -Fq 'redirects =' topology.nix
            grep -Fq 'sshKnownHosts = [' topology.nix
            grep -Fq 'git.example.test' topology.nix
            touch $out
          '';

        fleetix-lib-helpers = let
          topology = {
            schemaVersion = 2;
            links = {
              mesh = {
                subnet = "10.123.0.0/24";
                port = 51820;
                endpointSubdomain = "mesh";
              };
              direct-link.subnet = "10.10.0.0/24";
            };
            hosts = {
              atlas = {
                network = {
                  lanIp = "192.168.178.88";
                  directLinkIp = "10.10.0.1";
                };
                links.mesh = {
                  address = "10.123.0.5";
                  role = "server";
                  publicKey = "server-key";
                };
                links.direct-link.address = "10.10.0.1";
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
              dnsZones = [];
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
              pagesSites = [
                {
                  subdomain = "docs";
                  repository = "example/docs";
                  cnameTarget = "example.github.io";
                }
              ];
            };
            services.endpoints = {
              immich = {
                port = 2283;
                targetHost = "atlas";
                transport = "http";
                bind = "loopback";
                remoteVia = "immich-lan";
              };
              immich-lan = {
                port = 2283;
                targetHost = "atlas";
                transport = "http";
                bind = "lan";
              };
              ollama = {
                port = 11434;
                targetHost = "atlas";
                transport = "http";
                bind = "loopback";
              };
            };
            services.httpSites = {
              immich = {
                hostname = "immich.example.test";
                ingress = "public";
                access = "cloudflare";
                dnsPublication = "managed";
                routes = [
                  {
                    __pkl_class = "HttpRoute";
                    match = {
                      __pkl_class = "HttpMatch";
                      paths = [];
                      absentQueryParams = [];
                    };
                    action = {
                      __pkl_class = "ProxyAction";
                      type = "proxy";
                      endpoint = "immich";
                    };
                    responseHeaders = {};
                  }
                ];
              };
              ollama = {
                hostname = "ollama.internal.example.test";
                ingress = "vpn";
                access = "vpn";
                dnsPublication = "none";
                routes = [];
              };
              docs = {
                hostname = "docs.example.test";
                ingress = "public";
                access = "direct";
                dnsPublication = "managed";
                routes = [];
              };
            };
          };
          endpoint = self.lib.services.resolveEndpoint {
            inherit topology;
            endpointName = "immich";
            ingressHost = "nomad";
          };
          missingEndpoint = self.lib.services.resolveEndpoint {
            inherit topology;
            endpointName = "missing";
            require = false;
          };
          addressExcludes = self.lib.domains.dynamicHostAddressExcludes {
            inherit topology;
            zone = "example.test";
          };
          internalAddressExcludes = self.lib.domains.dynamicHostAddressExcludes {
            inherit topology;
            zone = "internal.example.test";
          };
          serviceIntents = self.lib.services.managedDnsCnameIntents {inherit topology;};
          pagesIntents = self.lib.services.pagesCnameIntents {inherit topology;};
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
            test "${endpoint.name}" = "immich-lan"
            test "${endpoint.targetHost}" = "atlas"
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
            test "${(builtins.elemAt serviceIntents 1).relativeName}" = "immich"
            test "${(builtins.elemAt serviceIntents 1).target}" = "example.test"
            test "${toString (builtins.elemAt serviceIntents 1).proxied}" = "1"
            test "${(builtins.elemAt pagesIntents 0).relativeName}" = "docs"
            test "${(builtins.elemAt pagesIntents 0).target}" = "example.github.io"
            test "${normalized.hosts.atlas.linkAddresses.mesh}" = "10.123.0.5"
            test "${toString (self.lib.links.hostsShareLink {
              inherit topology;
              linkName = "direct-link";
              hostNames = ["atlas" "nomad"];
            })}" = "1"
            test "${toString (self.lib.links.hostsShareLink {
              inherit topology;
              linkName = "mesh";
              hostNames = ["atlas" "nomad"];
            })}" = ""
            test "${normalized.domains.serviceHosts.immich}" = "immich.example.test"
            test "${normalized.links.mesh.serverAddress}" = "10.123.0.5"
            test "${normalized.services.endpointByName.immich.targetHost}" = "atlas"
            test "${(builtins.head normalized.services.siteByName.immich.routes).action.type}" = "proxy"
            test "${toString (builtins.hasAttr "__pkl_class" (builtins.head normalized.services.siteByName.immich.routes).action)}" = ""
            test "${(self.lib.services.endpointsForHost {
              inherit topology;
              hostName = "atlas";
            }).immich-lan.bind}" = "lan"
            touch $out
          '';

        module-integration-fixtures = let
          topology = {
            hosts.demo.system = "x86_64-linux";
            links.mesh = {subnet = "10.0.0.0/24";};
            domains.zones = ["example.test"];
            services = {};
          };
          nixos = nixpkgs.lib.evalModules {
            modules = [
              assertionsModule
              self.nixosModules.topology
              {
                fleetix.enable = true;
                fleetix.value = topology;
              }
            ];
          };
          integratedHome = nixpkgs.lib.evalModules {
            modules = [
              assertionsModule
              self.homeModules.topology
              {_module.args.osConfig = nixos.config;}
            ];
          };
          standaloneHome = nixpkgs.lib.evalModules {
            modules = [
              assertionsModule
              self.homeModules.topology
              {_module.args.osConfig = null;}
              {
                fleetix.enable = true;
                fleetix.value = topology;
              }
            ];
          };
        in
          pkgs.runCommand "fleetix-module-integration-fixtures" {} ''
            test "${toString (builtins.hasAttr "fleetix" nixos.config)}" = 1
            test "${nixos.config.fleetix.topology.hosts.demo.system}" = x86_64-linux
            test "${integratedHome.config.fleetix.topology.hosts.demo.system}" = x86_64-linux
            test "${standaloneHome.config.fleetix.topology.hosts.demo.system}" = x86_64-linux
            touch $out
          '';
      }
    );

    devShells = forSystems (
      system: let
        pkgs = pkgsFor system;
        toolchain = rs-harbor.lib.mkToolchain {
          inherit pkgs;
          toolchainProfile = "nightly";
        };
        cargoConfig = rs-harbor.lib.mkCargoConfig {inherit pkgs;};
        cross = rs-harbor.lib.mkCross {inherit pkgs system;};
        compatShell = channel: let
          toolchain = pkgs.rust-bin.stable.${channel}.default;
          cargoConfig = rs-harbor.lib.mkCargoConfig {
            inherit pkgs;
            channel = "stable";
          };
          cross = rs-harbor.lib.mkCross {
            inherit pkgs system;
            enableOsxcross = false;
          };
        in
          rs-harbor.lib.mkDevShell {
            inherit pkgs;
            craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);
            inherit cargoConfig cross;
            packages = [toolchain];
            enableWindowsEnv = false;
            enableOsxcrossEnv = false;
          };
      in
        (rs-harbor.lib.mkDevShells {
          inherit pkgs cross cargoConfig;
          inherit (toolchain) craneLib;
        })
        // {
          stable = compatShell "1.96.1";
          msrv = compatShell "1.88.0";
        }
    );
  };
}
