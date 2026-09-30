# Translate generic service facts to Gatus. No consumer domains or inventories.
{
  lib,
  fleetixLib,
}: let
  valueOr = value: fallback:
    if value == null
    then fallback
    else value;
  # Keep aligned with Gatus config/key/key.go. These are persisted history IDs.
  sanitize = value:
  # Nix lowercases ASCII only; do not generate keys with different case folding.
    assert lib.assertMsg (builtins.match "[ -~]*" value != null) "fleetix.gatus: monitor labels must be ASCII";
      lib.replaceStrings [" " "/" "_" "," "." "#" "+" "&"] ["-" "-" "-" "-" "-" "-" "-" "-"] (lib.toLower (lib.strings.trim value));
  key = group: name: "${sanitize group}_${sanitize name}";
  fail = message: throw "fleetix.gatus: ${message}";
  bracket = address:
    if lib.hasInfix ":" address
    then "[${address}]"
    else address;
in {
  inherit key;

  coverage = {topology}: let
    profiles = builtins.attrValues (topology.services.catalog or {});
    endpointNames = lib.concatMap (s: s.endpoints or []) profiles;
    siteNames = lib.concatMap (s: s.sites or []) profiles;
  in {
    unprofiledEndpoints = lib.subtractLists endpointNames (builtins.attrNames (topology.services.endpoints or {}));
    unprofiledSites = lib.subtractLists siteNames (builtins.attrNames (topology.services.httpSites or {}));
  };

  inventory = {
    topology,
    hostName,
    domains,
    domain,
    defaultDomain,
    includeInternal ? false,
  }: let
    catalog = topology.services.catalog or {};
    endpoints = topology.services.endpoints or {};
    sites = topology.services.httpSites or {};
    publicSite = name: let
      site = sites.${name} or {};
    in
      (site.access or "vpn")
      != "vpn"
      && ((topology.deployment.ingressGroups or {}).${site.ingress or ""}.scope or null) == "public";
    require = condition: message:
      if condition
      then true
      else fail message;
    # Also validate at the Nix boundary: consumers can supply a raw Nix topology
    # without running the Rust validator. Force checks even for unselected data.
    validateProfile = service: profile: let
      label = "profile ${service}";
      visibility = profile.visibility or "internal";
      health = profile.health or {};
      reason = profile.exclusionReason or null;
      validateCheck = id: check: let
        p = check.probe;
        site = p.site or null;
        endpoint = p.endpoint or null;
        target = endpoints.${endpoint} or {};
        public = valueOr (check.visibility or null) visibility == "public";
        hasHost = builtins.elem p.type ["dns" "unit" "job" "contract"];
        path = p.path or "/";
        unit = p.unit or "";
      in
        builtins.deepSeq [
          (require ((check.displayName or null) == null || lib.strings.trim check.displayName != "") "${label}/${id}: empty display label")
          (require ((check.category or null) == null || (lib.strings.trim check.category != "" && !lib.hasInfix "\"" check.category && !lib.hasInfix "\\" check.category)) "${label}/${id}: invalid category label")
          (require (builtins.elem (valueOr (check.visibility or null) visibility) ["public" "internal"]) "${label}/${id}: invalid visibility")
          (require (!(visibility == "internal" && public)) "${label}/${id}: visibility escalation")
          (require (builtins.elem p.type ["http" "tcp" "dns" "unit" "job" "contract"]) "${label}/${id}: unsupported probe type")
          (require ((check.intervalSeconds or 60) >= 10 && (check.timeoutSeconds or 10) > 0 && (check.timeoutSeconds or 10) <= (check.intervalSeconds or 60) && (check.maxResponseTimeMs or 5000) > 0) "${label}/${id}: invalid polling budget")
          (require (!public || (p.type == "http" && site != null && publicSite site)) "${label}/${id}: public summaries require a public HTTP site")
          (require (!hasHost || builtins.hasAttr (p.host or "") (topology.hosts or {})) "${label}/${id}: unknown host")
          (require (p.type != "http" || ((site != null) != (endpoint != null))) "${label}/${id}: HTTP needs exactly one site or endpoint")
          (require (site == null || (builtins.hasAttr site sites && builtins.elem site (profile.sites or []))) "${label}/${id}: unknown or unowned site")
          (require (endpoint == null || (builtins.hasAttr endpoint endpoints && builtins.elem endpoint (profile.endpoints or []))) "${label}/${id}: unknown or unowned endpoint")
          (require (p.type != "tcp" || (endpoint != null && (target.tcpProbe or true))) "${label}/${id}: endpoint forbids TCP health checks")
          (require (p.type != "http" || endpoint == null || builtins.elem (target.transport or "") ["http" "https"]) "${label}/${id}: unsupported HTTP transport")
          (require (p.type != "http" || endpoint == null || (target.tlsServerName or null) == null) "${label}/${id}: use a site for hostname-verified TLS")
          (require (p.type != "http" || (lib.hasPrefix "/" path && !lib.hasPrefix "//" path && !lib.hasInfix "\n" path && !lib.hasInfix "\r" path)) "${label}/${id}: invalid HTTP path")
          (require (p.type != "http" || ((p.acceptedStatus or [200]) != [] && lib.all (s: s >= 100 && s <= 599) (p.acceptedStatus or [200]))) "${label}/${id}: invalid HTTP statuses")
          (require (!builtins.elem p.type ["unit" "job"] || builtins.match "[A-Za-z0-9_@:.\\\\-]+\\.(service|socket|timer|target|mount)" unit != null) "${label}/${id}: invalid unit name")
          (require (p.type != "job" || (lib.hasSuffix ".service" unit && (p.maxAgeSeconds or 0) > 0)) "${label}/${id}: invalid job freshness contract")
          (require (p.type != "contract" || (p.contract or "") != "") "${label}/${id}: empty contract")
          (require (p.type != "dns" || ((p.query or "") != "" && (p.expected or "") != "")) "${label}/${id}: empty DNS contract")
        ]
        true;
    in
      builtins.deepSeq [
        (require (lib.strings.trim (profile.displayName or "") != "" && lib.strings.trim (profile.category or "") != "") "${label}: missing display labels")
        (require (lib.all (s: !lib.hasInfix "\"" s && !lib.hasInfix "\\" s) ([profile.displayName profile.category] ++ map (c: valueOr (c.displayName or null) "") (builtins.attrValues health))) "${label}: Gatus labels cannot contain quotes or backslashes")
        (require (builtins.elem visibility ["internal" "public"]) "${label}: invalid visibility")
        (require (builtins.elem (profile.lifecycle or "active") ["active" "on-demand" "planned" "retired"]) "${label}: invalid lifecycle")
        (require (reason == null || lib.strings.trim reason != "") "${label}: empty exclusion reason")
        (require (((profile.lifecycle or "active") == "active" && health != {}) || reason != null) "${label}: missing health policy or lifecycle reason")
        (require (lib.all (s: builtins.hasAttr s sites) (profile.sites or [])) "${label}: unknown site")
        (require (lib.all (e: builtins.hasAttr e endpoints) (profile.endpoints or [])) "${label}: unknown endpoint")
        (require (builtins.elem (affiliation profile) domains) "${label}: unregistered domain affiliation")
        (lib.mapAttrsToList validateCheck health)
      ]
      true;
    hostAddress = host:
      fleetixLib.hosts.resolveHostAddress {
        inherit topology;
        hostName = host;
        policy = ["wg-home" "lan"];
      };
    endpointAddress = name: let
      endpoint = topology.services.endpoints.${name} or (fail "unknown endpoint ${name}");
      address =
        if endpoint.bind == "loopback"
        then "127.0.0.1"
        else
          fleetixLib.hosts.resolveHostAddress {
            inherit topology;
            hostName = endpoint.targetHost;
            policy =
              if endpoint.bind == "vpn"
              then ["wg-home"]
              else ["lan"];
          };
    in "${bracket address}:${toString endpoint.port}";
    affiliation = profile: let
      siteDomains = lib.unique (map (site:
        fleetixLib.domains.zoneForHost {
          zones = domains;
          fqdn = topology.services.httpSites.${site}.hostname;
        }) (profile.sites or []));
    in
      if (profile.domain or null) != null
      then profile.domain
      else if siteDomains == []
      then defaultDomain
      else if builtins.length siteDomains == 1 && builtins.head siteDomains != null
      then builtins.head siteDomains
      else fail "${profile.displayName} needs an explicit domain affiliation";
    bodyCondition = assertion: let
      path = assertion.path or "";
      lhs = "[BODY]${lib.optionalString (path != "") ".${path}"}";
      value = assertion.value or "";
    in
      if assertion.operator == "equals"
      then "${lhs} == ${value}"
      else if assertion.operator == "contains"
      then "${lhs} == pat(*${value}*)"
      else if assertion.operator == "nonempty"
      then "len(${lhs}) > 0"
      else fail "unknown body assertion ${assertion.operator}";
    render = service: profile: id: check: let
      inherit (check) probe;
      visibility = valueOr (check.visibility or null) (profile.visibility or "internal");
      internal = visibility == "internal";
      selected =
        if internal
        then includeInternal
        else affiliation profile == domain;
      public = !includeInternal;
      name = valueOr (check.displayName or null) (
        profile.displayName + lib.optionalString (id != "health") " - ${id}"
      );
      group = valueOr (check.category or null) profile.category;
      base = {
        inherit name group;
        interval = "${toString (check.intervalSeconds or 60)}s";
      };
      endpoint =
        if builtins.elem probe.type ["http" "tcp"] && (probe.endpoint or null) != null
        then topology.services.endpoints.${probe.endpoint}
        else null;
      localNetwork = endpoint != null && endpoint.bind == "loopback" && endpoint.targetHost != hostName;
      url =
        if probe.type == "http"
        then
          (
            if (probe.site or null) != null
            then "https://${topology.services.httpSites.${probe.site}.hostname}"
            else if endpoint == null
            then fail "HTTP probe ${service}/${id} has no target"
            else "${
              if endpoint.transport == "https"
              then "https"
              else "http"
            }://${endpointAddress probe.endpoint}"
          )
          + (probe.path or "/")
        else if probe.type == "tcp"
        then "tcp://${endpointAddress probe.endpoint}"
        else "${bracket (hostAddress probe.host)}:53";
      network = builtins.elem probe.type ["http" "tcp" "dns"] && !localNetwork;
      conditions =
        (
          if probe.type == "http"
          then
            ["[STATUS] == any(${lib.concatMapStringsSep ", " toString (probe.acceptedStatus or [200])})"]
            ++ map bodyCondition (probe.body or [])
          else if probe.type == "dns"
          then ["[DNS_RCODE] == NOERROR" "[BODY] == ${probe.expected}"]
          else ["[CONNECTED] == true"]
        )
        ++ ["[RESPONSE_TIME] < ${toString (check.maxResponseTimeMs or 5000)}"];
      settings =
        base
        // {
          inherit url conditions;
          client =
            {timeout = "${toString (check.timeoutSeconds or 10)}s";}
            // lib.optionalAttrs (probe.type == "http") {ignore-redirect = !(probe.followRedirects or true);};
        }
        // lib.optionalAttrs (probe.type == "dns") {
          dns = {
            query-name = probe.query;
            query-type = "A";
          };
        }
        // lib.optionalAttrs public {
          ui = {
            hide-url = true;
            hide-hostname = true;
            hide-errors = true;
            dont-resolve-failed-conditions = true;
          };
        };
    in
      if (profile.visibility or "internal") == "internal" && !internal
      then fail "${service}/${id} publishes an internal service"
      else if !selected
      then []
      else if !network && builtins.match "[a-z0-9_-]+" (key group name) == null
      then fail "${service}/${id}: external-result keys must be URL-safe (Gatus does not unescape route parameters)"
      else [
        ({
            inherit service id visibility network;
            key = key group name;
            settings =
              if network
              then settings
              else {
                inherit name group;
                heartbeat.interval = "${toString (3 * (check.intervalSeconds or 60))}s";
              };
            source = check;
          }
          // lib.optionalAttrs (!network) {
            host =
              if localNetwork
              then endpoint.targetHost
              else probe.host;
          }
          // lib.optionalAttrs localNetwork {
            target = {
              host = endpoint.targetHost;
              inherit url;
            };
          })
      ];
    excluded = lib.concatMap (service: let
      profile = catalog.${service};
      reason = profile.exclusionReason or null;
    in
      lib.optional ((profile.lifecycle or "active") != "active" || (profile.health or {}) == {}) {
        inherit service;
        reason =
          if reason != null && reason != ""
          then reason
          else fail "${service} lacks a health policy or lifecycle reason";
      }) (builtins.attrNames catalog);
    excludedNames = map (s: s.service) excluded;
    monitors = lib.concatMap (service: let
      profile = catalog.${service};
    in
      lib.concatMap (id: render service profile id profile.health.${id}) (builtins.attrNames (profile.health or {})))
    (lib.subtractLists excludedNames (builtins.attrNames catalog));
    keys = map (m: m.key) monitors;
  in
    assert lib.assertMsg (builtins.length keys == builtins.length (lib.unique keys)) "fleetix.gatus: colliding monitor keys";
    assert lib.assertMsg (builtins.elem domain domains && builtins.elem defaultDomain domains) "fleetix.gatus: unregistered instance/default domain";
      builtins.deepSeq (lib.mapAttrsToList validateProfile catalog) (builtins.deepSeq excluded {
        inherit monitors excluded;
        endpoints = map (m: m.settings) (builtins.filter (m: m.network) monitors);
        externalChecks = builtins.filter (m: !m.network) monitors;
      });
}
