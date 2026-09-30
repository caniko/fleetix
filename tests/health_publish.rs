use fleetix::health::{PublisherConfig, collect, publish};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn config(url: String) -> PublisherConfig {
    serde_json::from_value(json!({
        "host": "hub", "url": url,
        "systemctl": "/missing-systemctl", "timeout": "/missing-timeout",
        "checks": [{"key": "storage_backup-·-job", "source": {
            "probe": {"type": "unit", "host": "hub", "unit": "worker.service"}
        }}]
    }))
    .unwrap()
}

#[test]
fn probe_execution_failure_is_an_unhealthy_observation() {
    let config = config("http://127.0.0.1:1/".into());
    let results = collect(&config, "hub", 0, |_, _| unreachable!()).unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0].healthy);
    assert!(!results[0].detail.contains("missing-timeout"));
    assert!(collect(&config, "other-host", 0, |_, _| unreachable!()).is_err());
}

#[tokio::test]
async fn authenticates_encoded_external_results_and_rejects_redirects() {
    for status in [
        "200 OK",
        "302 Found",
        "401 Unauthorized",
        "500 Internal Server Error",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let config = config(format!("http://{address}/"));
        let observations = collect(&config, "hub", 0, |_, _| unreachable!()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut received = Vec::new();
            let mut buf = [0; 4096];
            while !received.windows(4).any(|p| p == b"\r\n\r\n") {
                let count = stream.read(&mut buf).unwrap();
                assert!(count > 0 && received.len() < 16384);
                received.extend_from_slice(&buf[..count]);
            }
            write!(stream, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\nLocation: http://{address}/redirected\r\n\r\n").unwrap();
            let request = String::from_utf8(received).unwrap().to_lowercase();
            assert!(request.starts_with(
                "post /api/v1/endpoints/storage_backup-%c2%b7-job/external?success=false&error="
            ));
            assert!(request.contains("authorization: bearer synthetic-test-token\r\n"));
            assert!(!request.contains("missing-timeout"));
            listener
        });
        let result = publish(&config, "synthetic-test-token", &observations).await;
        assert_eq!(result.is_ok(), status == "200 OK");
        let listener = server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[tokio::test]
async fn invalid_tail_is_rejected_before_any_result_is_published() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(format!("http://{}/", listener.local_addr().unwrap()));
    let mut observations = collect(&config, "hub", 0, |_, _| unreachable!()).unwrap();
    observations.push(fleetix::health::Observation {
        key: "not-selected".into(),
        healthy: true,
        detail: "healthy".into(),
    });
    assert!(
        publish(&config, "synthetic-test-token", &observations)
            .await
            .is_err()
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn local_http_readiness_checks_status_and_body_without_relaying() {
    for body in [r#"{"ready":true}"#, r#"{"ready":false}"#] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config("http://127.0.0.1:1/".into());
        config.checks = serde_json::from_value(json!([{
            "key": "apps_ready",
            "target": {"host": "hub", "url": format!("http://{}/ready", listener.local_addr().unwrap())},
            "source": {"probe": {"type": "http", "endpoint": "worker", "body": [{"path": "ready", "operator": "equals", "value": "true"}]}}
        }])).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert!(String::from_utf8_lossy(&buffer[..count]).starts_with("GET /ready "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let observations = collect(&config, "hub", 0, |_, _| unreachable!()).unwrap();
        assert_eq!(observations[0].healthy, body.contains("true"));
        server.join().unwrap();
    }
}

#[test]
fn local_network_targets_must_belong_to_this_host_and_use_literal_loopback() {
    for (host, url) in [
        ("other", "http://127.0.0.1:80/"),
        ("hub", "http://example.test/"),
        ("hub", "http://user@127.0.0.1/"),
    ] {
        let mut config = config("http://127.0.0.1:1/".into());
        config.checks = serde_json::from_value(json!([{
            "key": "apps_ready", "target": {"host": host, "url": url},
            "source": {"probe": {"type": "http", "endpoint": "worker"}}
        }]))
        .unwrap();
        assert!(collect(&config, "hub", 0, |_, _| unreachable!()).is_err());
    }
}
