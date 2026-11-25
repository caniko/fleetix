#![cfg(feature = "pkl")]

use fleetix::{Topology, validate};

#[test]
fn authored_pkl_cloud_contracts_load_through_the_real_schema() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Schema.pkl"),
        include_str!("../lib/topology/Schema.pkl"),
    )
    .unwrap();
    let source = dir.path().join("Topology.pkl");
    std::fs::write(&source, r#"
import "Schema.pkl" as S
schemaVersion = 2
links = new {}
hosts = new {
  ["edge"] = new S.Host {
    system = "x86_64-linux"
    hostPubkey = "ssh-ed25519 fixture-key"
    management { publicAddress = "192.0.2.10"; sshPort = 1337 }
  }
}
domains = new S.Domains {
  zones = new Listing { "example.test" }
  dnsZones { new S.DnsZone { name = "example.test" } }
  dynamicHosts { new S.DynamicHost { fqdn = "wg.example.test" } }
  publicationTargets {
    ["edge"] = new S.PublicationTarget {
      hostname = "edge.example.test"
      targetHost = "edge"
      ipv4 = "192.0.2.10"
    }
  }
}
services = new S.Services {
  httpSites {
    ["media"] = new S.HttpSite {
      hostname = "media.example.test"
      ingress = "public"
      access = "direct"
      dnsPublication = "managed"
      publicationTarget = "edge"
      routes = new Listing { new S.HttpRoute { match = new S.HttpMatch {}; action = new S.RespondAction { status = 200 } } }
    }
  }
}
deployment = new S.Deployment {
  ingressGroups { ["public"] = new S.IngressGroup { scope = "public"; hosts = new Listing { "edge" } } }
}
"#).unwrap();
    let topology: Topology = fleetix::pkl::load_sync(&source).unwrap();
    assert!(validate(&topology).is_ok());
    assert_eq!(topology.hosts["edge"].management.ssh_port, Some(1337));
    assert_eq!(
        topology.service_cname_intents()[0].target,
        "edge.example.test"
    );
    assert_eq!(topology.publication_address_intents()[0].ipv4, "192.0.2.10");
    assert!(topology.publication_address_intents()[0].ipv6.is_none());
    assert_eq!(topology.zone_provider("example.test"), Some("cloudflare"));
    assert_eq!(
        topology.dynamic_host_address_excludes("example.test").len(),
        2
    );
}

#[test]
fn authored_pkl_dns_migration_loads_through_the_real_schema() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Schema.pkl"),
        include_str!("../lib/topology/Schema.pkl"),
    )
    .unwrap();
    let source = dir.path().join("Topology.pkl");
    std::fs::write(
        &source,
        r#"
import "Schema.pkl" as S
schemaVersion = 2
links = new {}
hosts = new {}
services = new {}
domains = new S.Domains {
  zones { "example.test" }
  dnsProviders = new Mapping {
    ["primary"] = new S.DnsProvider { kind = "desec" }
    ["next"] = new S.DnsProvider { kind = "hetzner" }
  }
  dnsZones { new S.DnsZone { name = "example.test"; provider = "primary"; migrateTo = "next" } }
  dynamicHosts { new S.DynamicHost { fqdn = "wg.example.test"; families = new Listing { "ipv4" } } }
}
accessMode: S.HttpAccess = "proxied"
"#,
    )
    .unwrap();
    let value: serde_json::Value = fleetix::pkl::load_sync(&source).unwrap();
    assert_eq!(value["accessMode"], "proxied");
    assert!(value["domains"]["dnsProviders"].get("cloudflare").is_none());
    assert_eq!(value["domains"]["dnsProviders"]["next"]["kind"], "hetzner");
    let topology: Topology = serde_json::from_value(value).unwrap();
    assert!(validate(&topology).is_ok());
    let updaters = topology.dynamic_updaters();
    assert_eq!(updaters["primary"][0].fqdn, "wg.example.test");
    assert_eq!(
        updaters["next"][0].families,
        [fleetix::topology::IpFamily::Ipv4]
    );
}

#[test]
fn gpu_passthrough_loads_through_the_embedded_evaluator() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Schema.pkl"),
        include_str!("../lib/topology/Schema.pkl"),
    )
    .unwrap();
    let source = dir.path().join("Topology.pkl");
    std::fs::write(&source, r#"
import "Schema.pkl" as S
schemaVersion = 2
links = new {}
domains = new {}
services = new {}
hosts = new {
  ["desktop"] = new S.Host {
    system = "x86_64-linux"
    gpu = new S.Gpu {
      igpu = "amd"
      passthrough { ["igpu"] = new S.GpuPassthrough { address = "0000:7d:00.0"; id = "1002:13c0"; vendor = "amd"; rom = "vfct" } }
    }
  }
}
"#).unwrap();
    let topology: Topology = fleetix::pkl::load_sync(&source).unwrap();
    assert!(validate(&topology).is_ok());
    let igpu = &topology.hosts["desktop"].gpu.passthrough["igpu"];
    assert_eq!(igpu.rom, fleetix::topology::GpuPassthroughRom::Vfct);
    assert!(!igpu.repeatable);
}
