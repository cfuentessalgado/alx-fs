use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use alx::parse_bind_address;

#[test]
fn binding_defaults_to_localhost_and_accepts_an_explicit_ip() {
    assert_eq!(
        parse_bind_address(None, false).unwrap(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000)
    );
    assert_eq!(
        parse_bind_address(Some("0.0.0.0:8080"), false).unwrap(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080)
    );
    assert_eq!(
        parse_bind_address(Some("127.0.0.1:0"), false).unwrap(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    );
}

#[test]
fn binding_rejects_conflicts_and_hostnames() {
    assert!(parse_bind_address(Some("127.0.0.1:3000"), true).is_err());
    assert!(parse_bind_address(Some("localhost:3000"), false).is_err());
}
