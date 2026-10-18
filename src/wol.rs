use std::{fmt, net::Ipv4Addr, str::FromStr};

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MacAddress(pub [u8; 6]);

impl FromStr for MacAddress {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts: Vec<_> = value.split([':', '-']).collect();
        if parts.len() != 6 || parts.iter().any(|p| p.len() != 2) {
            return Err("MAC must contain six hex octets, e.g. 44:27:45:51:7E:9A".into());
        }
        let mut bytes = [0; 6];
        for (out, part) in bytes.iter_mut().zip(parts) {
            *out = u8::from_str_radix(part, 16).map_err(|_| "Invalid MAC hex digit")?;
        }
        if bytes == [0; 6] || bytes[0] & 1 != 0 {
            return Err("MAC must be a nonzero unicast address".into());
        }
        Ok(Self(bytes))
    }
}
impl TryFrom<String> for MacAddress {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<MacAddress> for String {
    fn from(mac: MacAddress) -> Self {
        mac.to_string()
    }
}
impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

pub fn magic_packet(mac: MacAddress) -> [u8; 102] {
    let mut packet = [0xff; 102];
    for chunk in packet[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&mac.0);
    }
    packet
}

pub async fn send(
    mac: MacAddress,
    tv_ip: Ipv4Addr,
    broadcast: Ipv4Addr,
) -> std::io::Result<Ipv4Addr> {
    send_packets(mac, tv_ip, (broadcast, 9).into()).await
}

async fn send_packets(
    mac: MacAddress,
    tv_ip: Ipv4Addr,
    destination: std::net::SocketAddr,
) -> std::io::Result<Ipv4Addr> {
    // UDP connect selects the OS route/source without transmitting any packet.
    // Re-evaluate on each bounded wake burst because resume can change routes.
    let route = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    route.connect((tv_ip, 9)).await?;
    let source = match route.local_addr()?.ip() {
        std::net::IpAddr::V4(ip) => ip,
        _ => return Err(std::io::Error::other("Expected an IPv4 wake route")),
    };
    let socket = UdpSocket::bind((source, 0)).await?;
    socket.set_broadcast(true)?;
    let packet = magic_packet(mac);
    // A short burst, only on explicit ON. No background traffic.
    for _ in 0..3 {
        socket.send_to(&packet, destination).await?;
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn udp_burst_uses_selected_route_and_delivers_three_packets() {
        let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mac: MacAddress = "44:27:45:51:7E:9A".parse().unwrap();
        let source = send_packets(mac, Ipv4Addr::LOCALHOST, receiver.local_addr().unwrap())
            .await
            .unwrap();
        assert_eq!(source, Ipv4Addr::LOCALHOST);
        for _ in 0..3 {
            let mut bytes = [0; 256];
            let (length, peer) = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                receiver.recv_from(&mut bytes),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(peer.ip(), Ipv4Addr::LOCALHOST);
            assert_eq!(&bytes[..length], &magic_packet(mac));
        }
    }
}
