//! A listening port stays ours against a `SO_REUSEADDR` socket (Core #36169,
//! #36305).

use std::net::Ipv4Addr;
#[cfg(windows)]
use std::net::SocketAddr;

use rbitcoin_net::listen::bind_tcp;
use tokio::net::TcpSocket;

/// Windows only: a default listener there lets a `SO_REUSEADDR` socket from
/// the same user bind a specific address on its wildcard port. BSD allows
/// that bind by design.
#[cfg(windows)]
#[tokio::test]
async fn reuseaddr_specific_bind_cannot_take_a_wildcard_listen_port() {
    let listener = bind_tcp((Ipv4Addr::UNSPECIFIED, 0).into()).expect("bind");
    let port = listener.local_addr().unwrap().port();

    let hijack = TcpSocket::new_v4().unwrap();
    hijack.set_reuseaddr(true).unwrap();
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    assert!(
        hijack.bind(target).is_err(),
        "a SO_REUSEADDR socket bound {target} while 0.0.0.0:{port} was listening"
    );
}

#[cfg(unix)]
#[test]
fn listener_fd_reserve_keeps_a_floor_and_a_cap() {
    use rbitcoin_net::listen::listener_fd_reserve;
    assert_eq!(listener_fd_reserve(1280), 160);
    assert_eq!(listener_fd_reserve(1281), 1024);
    assert_eq!(listener_fd_reserve(1024), 128);
    assert_eq!(listener_fd_reserve(248), 32);
}

/// `feature_port.py`: another process cannot bind the P2P listen port.
#[tokio::test]
async fn reuseaddr_bind_cannot_take_a_listen_port() {
    let listener = bind_tcp((Ipv4Addr::UNSPECIFIED, 0).into()).expect("bind");
    let target = listener.local_addr().unwrap();

    let hijack = TcpSocket::new_v4().unwrap();
    hijack.set_reuseaddr(true).unwrap();
    assert!(
        hijack.bind(target).is_err(),
        "a SO_REUSEADDR socket bound {target} while it was listening"
    );
}
