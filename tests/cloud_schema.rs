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
}
