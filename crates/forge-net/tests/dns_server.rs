//! The resolver over real sockets on high ports (port 53 itself is exercised by the manual
//! test of docs/06 §5.4): A/AAAA inside the zones, NXDOMAIN outside, UDP and TCP, port
//! fallback, remote zones, reload, stats, robustness.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket as StdUdp};
use std::time::Duration;

use forge_core::actor::ActorSupervisor;
use forge_core::event::EventBus;
use forge_net::dns::{DnsServer, DnsServerConfig, Transport, ZoneSet, query, resolve};
use hickory_server::proto::op::ResponseCode;
use hickory_server::proto::rr::RecordType;

const T: Duration = Duration::from_secs(3);
const LOCALHOST_V4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const LOCALHOST_V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);
const FALLBACK_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 53);

fn supervisor() -> ActorSupervisor {
    ActorSupervisor::new(EventBus::new())
}

fn zones() -> ZoneSet {
    ZoneSet::local(&[".test".into()]).unwrap()
}

/// A port usable for both UDP and TCP on `ip` right now. A bare ephemeral UDP port may
/// fall in a TCP range Hyper-V excludes (bind fails with WSAEACCES), so a throwaway
/// server on port 0 finds one (`bind` retries until both protocols accept the port).
async fn free_port(ip: Ipv4Addr) -> u16 {
    let probe = DnsServer::spawn(
        &supervisor(),
        DnsServerConfig::on(vec![(ip, 0).into()]),
        ZoneSet::new(),
    );
    let port = probe.start().await.unwrap().port();
    probe.shutdown().await;
    port
}

/// A server on an ephemeral port of 127.0.0.1.
async fn running(zones: ZoneSet) -> (DnsServer, SocketAddr) {
    let config = DnsServerConfig::on(vec![(Ipv4Addr::LOCALHOST, 0).into()]);
    let server = DnsServer::spawn(&supervisor(), config, zones);
    let addr = server.start().await.unwrap();
    (server, addr)
}

#[tokio::test]
async fn answers_the_zone_and_nxdomain_outside() {
    let (server, addr) = running(zones()).await;
    assert_ne!(addr.port(), 0);

    for transport in [Transport::Udp, Transport::Tcp] {
        let a = query(addr, "qualquer.test", RecordType::A, transport, T)
            .await
            .unwrap();
        assert_eq!(a.code, ResponseCode::NoError, "{transport:?}");
        assert!(a.authoritative);
        assert_eq!(a.addresses(), vec![LOCALHOST_V4], "{transport:?}");
        assert!(a.answers[0].ttl <= 60, "short TTL");

        let aaaa = query(addr, "api.loja.test", RecordType::AAAA, transport, T)
            .await
            .unwrap();
        assert_eq!(aaaa.addresses(), vec![LOCALHOST_V6], "{transport:?}");

        let outside = query(addr, "x.com", RecordType::A, transport, T)
            .await
            .unwrap();
        assert_eq!(outside.code, ResponseCode::NXDomain, "{transport:?}");
        assert!(outside.answers.is_empty());
        assert!(!outside.authoritative);

        let mx = query(addr, "loja.test", RecordType::MX, transport, T)
            .await
            .unwrap();
        assert_eq!(mx.code, ResponseCode::NoError);
        assert!(mx.answers.is_empty());
        assert_eq!(mx.authorities.len(), 1, "SOA for negative caching");
    }
    assert_eq!(
        resolve(addr, "LOJA.TEST", T).await.unwrap(),
        vec![LOCALHOST_V4, LOCALHOST_V6]
    );
    assert!(resolve(addr, "x.com", T).await.unwrap().is_empty());

    let stats = server.stats().await.unwrap();
    assert!(stats.running);
    assert_eq!(stats.listen, Some(addr));
    assert!(!stats.nrpt_capable, "not port 53");
    assert_eq!(stats.zones, vec!["test".to_owned()]);
    // 4 questions per transport + 2 resolves of 2 questions each.
    assert_eq!(stats.queries, 12);
    assert_eq!(stats.answered, 6);
    assert_eq!(stats.nxdomain, 4);
    assert_eq!(stats.nodata, 2);
    assert_eq!(stats.errors, 0);
    assert_eq!(stats.last_error, None);
    server.shutdown().await;
}

#[tokio::test]
async fn falls_back_to_the_next_address_when_taken() {
    // Take the UDP port on 127.0.0.1 and keep it: the first candidate is busy.
    let port = free_port(FALLBACK_IP).await;
    let _squatter = StdUdp::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let config = DnsServerConfig::on(vec![
        (Ipv4Addr::LOCALHOST, port).into(),
        (FALLBACK_IP, port).into(),
    ]);
    let server = DnsServer::spawn(&supervisor(), config, zones());
    let addr = server.start().await.unwrap();
    assert_eq!(addr, SocketAddr::from((FALLBACK_IP, port)));
    assert_eq!(
        resolve(addr, "qualquer.test", T).await.unwrap(),
        vec![LOCALHOST_V4, LOCALHOST_V6]
    );
    // start is idempotent.
    assert_eq!(server.start().await.unwrap(), addr);
    server.shutdown().await;
}

#[tokio::test]
async fn reports_port_in_use_when_every_address_is_taken() {
    let port = free_port(Ipv4Addr::LOCALHOST).await;
    let squatter = StdUdp::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let squatter2 = StdUdp::bind((FALLBACK_IP, port)).unwrap();
    let config = DnsServerConfig::on(vec![
        (Ipv4Addr::LOCALHOST, port).into(),
        (FALLBACK_IP, port).into(),
    ]);
    let server = DnsServer::spawn(&supervisor(), config, zones());
    let err = server.start().await.unwrap_err();
    assert_eq!(err.code(), "port_in_use", "{err}");
    assert!(err.to_string().contains("127.0.0.53"), "{err}");
    let stats = server.stats().await.unwrap();
    assert!(!stats.running);
    assert!(stats.last_error.is_some());

    // Once the address is free again, start works.
    drop(squatter);
    drop(squatter2);
    let addr = server.start().await.unwrap();
    assert_eq!(addr.port(), port);
    assert_eq!(server.stats().await.unwrap().last_error, None);
    server.shutdown().await;
}

#[tokio::test]
async fn remote_zones_and_reload() {
    let mut zones = zones();
    zones
        .add_remote("hml.test", "10.0.0.5".parse().unwrap())
        .unwrap();
    let (server, addr) = running(zones).await;
    assert_eq!(
        resolve(addr, "loja.hml.test", T).await.unwrap(),
        vec!["10.0.0.5".parse::<IpAddr>().unwrap()],
        "remote zone wins, AAAA is NODATA"
    );
    let aaaa = query(addr, "loja.hml.test", RecordType::AAAA, Transport::Udp, T)
        .await
        .unwrap();
    assert_eq!(aaaa.code, ResponseCode::NoError);
    assert_eq!(
        resolve(addr, "loja.test", T).await.unwrap(),
        vec![LOCALHOST_V4, LOCALHOST_V6]
    );

    // A new remote appears: no restart, no socket change.
    let mut next = server.zones().as_ref().clone();
    next.add_remote("vps.test", "192.168.0.20".parse().unwrap())
        .unwrap();
    next.add_remote("vps.test", "fd00::20".parse().unwrap())
        .unwrap();
    server.reload(next).await.unwrap();
    assert_eq!(
        resolve(addr, "app.vps.test", T).await.unwrap(),
        vec![
            "192.168.0.20".parse::<IpAddr>().unwrap(),
            "fd00::20".parse().unwrap()
        ]
    );
    assert_eq!(server.stats().await.unwrap().listen, Some(addr));
    assert_eq!(server.zones().names()[..2], ["hml.test", "vps.test"]);

    // Dropping every zone: everything is NXDOMAIN.
    server.reload(ZoneSet::new()).await.unwrap();
    let a = query(addr, "loja.test", RecordType::A, Transport::Udp, T)
        .await
        .unwrap();
    assert_eq!(a.code, ResponseCode::NXDomain);
    server.shutdown().await;
}

#[tokio::test]
async fn stop_releases_the_sockets_and_start_binds_again() {
    let (server, addr) = running(zones()).await;
    server.stop().await.unwrap();
    let stats = server.stats().await.unwrap();
    assert!(!stats.running && stats.listen.is_none());
    // The exact address can be bound by someone else now.
    let probe = StdUdp::bind(addr).unwrap();
    drop(probe);
    assert!(
        query(
            addr,
            "x.test",
            RecordType::A,
            Transport::Udp,
            Duration::from_millis(300)
        )
        .await
        .is_err(),
        "nobody answers after stop"
    );
    let again = server.start().await.unwrap();
    assert!(
        resolve(again, "x.test", T)
            .await
            .unwrap()
            .contains(&LOCALHOST_V4)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn survives_garbage_and_answers_many_clients_at_once() {
    let (server, addr) = running(zones()).await;
    // Garbage, an empty datagram and a truncated header.
    let junk = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    for payload in [&b"not dns at all"[..], b"", &[0x12, 0x34, 0x01]] {
        junk.send_to(payload, addr).await.unwrap();
    }
    // A client that sends and goes away (ICMP port unreachable on Windows).
    let gone = StdUdp::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut msg = hickory_server::proto::op::Message::query();
    msg.add_query(hickory_server::proto::op::Query::query(
        "gone.test.".parse().unwrap(),
        RecordType::A,
    ));
    gone.send_to(&msg.to_vec().unwrap(), addr).unwrap();
    drop(gone);

    let tasks: Vec<_> = (0..200)
        .map(|i| {
            tokio::spawn(async move {
                let transport = if i % 4 == 0 {
                    Transport::Tcp
                } else {
                    Transport::Udp
                };
                query(addr, &format!("site{i}.test"), RecordType::A, transport, T).await
            })
        })
        .collect();
    for task in tasks {
        let reply = task.await.unwrap().unwrap();
        assert_eq!(reply.addresses(), vec![LOCALHOST_V4]);
    }
    let stats = server.stats().await.unwrap();
    assert!(stats.running, "still serving: {stats:?}");
    assert!(stats.answered >= 200);
    server.shutdown().await;
}

#[tokio::test]
async fn ipv6_loopback_listener() {
    let config = DnsServerConfig::on(vec![(Ipv6Addr::LOCALHOST, 0).into()]);
    let server = DnsServer::spawn(&supervisor(), config, zones());
    let Ok(addr) = server.start().await else {
        eprintln!("IPv6 loopback unavailable; skipping");
        return;
    };
    assert_eq!(
        resolve(addr, "loja.test", T).await.unwrap(),
        vec![LOCALHOST_V4, LOCALHOST_V6]
    );
    server.shutdown().await;
}
