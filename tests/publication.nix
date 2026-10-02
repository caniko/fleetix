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
  cnames = fleet.services.managedDnsCnameIntents {inherit topology;};
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
in
  assert (builtins.head cnames).target == "edge.example.test";
  assert !(builtins.head cnames).proxied;
  assert (builtins.head addresses).ipv4 == "192.0.2.10";
  assert (builtins.head addresses).ipv6 == null;
  assert fleet.services.managedDnsCnameIntents {topology = apex;} == [];
  assert map (intent: intent.relativeName) (fleet.domains.publicationAddressIntents {topology = apex;}) == ["edge" "@"];
  assert (builtins.head (fleet.services.managedDnsCnameIntents {topology = legacy;})).target == "example.test";
  assert (builtins.head (fleet.services.pagesCnameIntents {inherit topology;})).target == "docs.codeberg.page";
  assert !(builtins.tryEval (builtins.deepSeq (fleet.services.managedDnsCnameIntents {topology = unknown;}) true)).success; true
