{lib}: let
  fleet = import ../lib {inherit lib;};
  topology = {
    hosts.hub.network.lanIp = "192.0.2.1";
    deployment.ingressGroups.edge.scope = "public";
    services = {
      endpoints.database = {
        targetHost = "hub";
        port = 5432;
        bind = "loopback";
        transport = "tcp";
      };
      httpSites = {
        web = {
          hostname = "nested.app.example.test";
          access = "direct";
          ingress = "edge";
        };
        impostor.hostname = "badexample.test";
      };
      catalog = {
        web = {
          displayName = "Website";
          category = "Applications";
          visibility = "public";
          sites = ["web"];
          health = {
            ready.probe = {
              type = "http";
              site = "web";
              path = "/ready";
            };
            details = {
              category = "Identity";
              visibility = "internal";
              probe = {
                type = "http";
                site = "web";
                path = "/details";
              };
            };
          };
        };
        database = {
          displayName = "Database";
          category = "Storage";
          endpoints = ["database"];
          health.health.probe = {
            type = "tcp";
            endpoint = "database";
          };
        };
        retired = {
          displayName = "Retired";
          category = "Applications";
          lifecycle = "retired";
          exclusionReason = "Replaced";
        };
      };
    };
  };
  render = extra:
    fleet.gatus.inventory ({
        inherit topology;
        hostName = "hub";
        domains = ["example.test" "private.test"];
        defaultDomain = "private.test";
        domain = "example.test";
      }
      // extra);
  public = render {};
  private = render {
    domain = "private.test";
    includeInternal = true;
  };
  remote = render {
    hostName = "remote";
    domain = "private.test";
    includeInternal = true;
  };
  rejects = extra: !(builtins.tryEval (builtins.deepSeq (render extra) true)).success;
in
  # Match Gatus v5.36's config/key.ConvertGroupAndNameToKey, including case.
  assert fleet.gatus.key " Storage " "Database_A/B.C,+&#" == "storage_database-a-b-c----";
  assert map (e: e.name) public.endpoints == ["Website - ready"];
  assert (builtins.head public.endpoints).url == "https://nested.app.example.test/ready";
  assert (builtins.head public.endpoints).ui.hide-url;
  assert map (e: e.name) private.endpoints == ["Database" "Website - details"];
  assert (builtins.head private.endpoints).url == "tcp://127.0.0.1:5432";
  assert (builtins.elemAt private.endpoints 1).group == "Identity";
  assert map (e: e.service) public.excluded == ["retired"];
  assert fleet.gatus.coverage {inherit topology;}
  == {
    unprofiledEndpoints = [];
    unprofiledSites = ["impostor"];
  };
  assert map (c: c.service) remote.externalChecks == ["database"];
  assert (builtins.head remote.externalChecks).host == "hub";
  assert (builtins.head remote.externalChecks).target
  == {
    host = "hub";
    url = "tcp://127.0.0.1:5432";
  };
  assert rejects {topology = lib.recursiveUpdate topology {services.catalog.database.health.health.visibility = "public";};};
  assert rejects {topology = lib.recursiveUpdate topology {services.catalog.web.domain = "typo.test";};};
  assert rejects {topology = lib.recursiveUpdate topology {services.catalog.web.health.ready.probe.site = "impostor";};};
  assert rejects {topology = lib.recursiveUpdate topology {services.catalog.web.health.ready.probe.type = "typo";};};
  assert rejects {topology = lib.recursiveUpdate topology {services.catalog.web.health.ready.intervalSeconds = 0;};};
  assert rejects {topology = lib.recursiveUpdate topology {services.catalog.web.health.ready.probe.endpoint = "database";};};
  assert rejects {topology = lib.recursiveUpdate topology {services.catalog.duplicate = topology.services.catalog.web // {displayName = "website";};};};
  assert rejects {topology = lib.recursiveUpdate topology {services.httpSites.web.access = "vpn";};}; true
