{lib}: let
  fleet = import ../lib {inherit lib;};
  site = {
    hostname = "media.example.test";
    access = "direct";
    dnsPublication = "managed";
    publicationTarget = "edge";
  };
  topology = {
    domains = {
      zones = ["example.test"];
      publicationTargets.edge = {
        hostname = "edge.example.test";
        targetHost = "edge";
        ipv4 = "192.0.2.10";
      };
      pagesSites = [
        {
          subdomain = "docs";
          repository = "owner/docs";
          cnameTarget = "docs.codeberg.page";
        }
      ];
    };
    services.httpSites.media = site;
  };
  intents = topology: fleet.services.managedDnsCnameIntents {inherit topology;};
  cnames = intents topology;
  addresses = fleet.domains.publicationAddressIntents {inherit topology;};
  apex = topology // {services.httpSites.media = site // {hostname = "example.test";};};
  legacy =
    topology
    // {
      services.httpSites.media =
        site
        // {
          publicationTarget = null;
          access = "cloudflare";
        };
    };
  unknown = topology // {services.httpSites.media = site // {publicationTarget = "absent";};};
  lan = topology // {services.httpSites.media = legacy.services.httpSites.media // {access = "lan";};};
  proxied = legacy // {services.httpSites.media = legacy.services.httpSites.media // {access = "proxied";};};
  multi =
    proxied
    // {
      domains =
        proxied.domains
        // {
          zones = ["example.test" "internal.example.test"];
          dnsProviders = {
            edge.kind = "cloudflare";
            primary.kind = "desec";
          };
          dnsZones = [
            {
              name = "example.test";
              provider = "edge";
            }
            {
              name = "internal.example.test";
              provider = "primary";
              migrateTo = "edge";
            }
          ];
          dynamicHosts = [
            {
              fqdn = "wg.internal.example.test";
              families = ["ipv4"];
            }
          ];
        };
    };
  ddns = fleet.domains.dynamicUpdaters {topology = multi;};
  zones = multi.domains.dnsZones;
  withZones = dnsZones: multi // {domains = multi.domains // {inherit dnsZones;};};
  editPrimary = attrs: withZones ([(builtins.head zones // attrs)] ++ builtins.tail zones);
  migration = editPrimary {migrateTo = "primary";};
  nonProxy = editPrimary {provider = "primary";};
  external =
    nonProxy
    // {
      domains = nonProxy.domains // {zones = [];};
      services.httpSites.media = proxied.services.httpSites.media // {dnsPublication = "external";};
    };
  dnsFails = invalid: !(builtins.tryEval (builtins.deepSeq (intents invalid) true)).success;
in
  assert (builtins.head cnames).target == "edge.example.test";
  assert !(builtins.head cnames).proxied;
  assert (builtins.head addresses).ipv4 == "192.0.2.10";
  assert (builtins.head addresses).ipv6 == null;
  assert intents apex == [];
  assert map (intent: intent.relativeName) (fleet.domains.publicationAddressIntents {topology = apex;}) == ["edge" "@"];
  assert (builtins.head (intents legacy)).target == "example.test";
  assert intents lan == [];
  assert (builtins.head (fleet.services.pagesCnameIntents {inherit topology;})).target == "docs.codeberg.page";
  assert dnsFails unknown;
  assert (builtins.head (intents legacy)).proxied;
  assert (builtins.head (intents multi)).proxied;
  assert fleet.domains.zoneProvider {
    topology = legacy;
    zone = "example.test";
  }
  == "cloudflare";
  assert fleet.domains.zoneProvider {
    topology = multi;
    zone = "internal.example.test";
  }
  == "primary";
  assert fleet.domains.zoneProvider {
    topology = multi;
    zone = "unknown.test";
  }
  == null;
  assert (fleet.domains.dynamicUpdaters {topology.domains = legacy.domains // {dynamicHosts = [{fqdn = "wg.example.test";}];};}).cloudflare
  == [
    {
      fqdn = "wg.example.test";
      families = ["ipv4" "ipv6"];
    }
  ];
  assert ddns.edge == multi.domains.dynamicHosts;
  assert ddns.primary == ddns.edge;
  assert fleet.domains.dynamicHostAddressExcludes {
    topology = multi;
    zone = "internal.example.test";
  }
  == [
    {
      name = "wg";
      type = "A";
    }
  ];
  assert dnsFails migration;
  assert dnsFails nonProxy;
  assert !(builtins.head (intents (nonProxy
    // {
      services.httpSites.media = proxied.services.httpSites.media // {access = "direct";};
    }))).proxied;
  assert dnsFails external;
  assert builtins.all (invalidZone:
    dnsFails (withZones (zones
      ++ [
        ({
            name = "unused.test";
            provider = "edge";
          }
          // invalidZone)
      ])))
  [{provider = "missing";} {migrateTo = "missing";}]; true
