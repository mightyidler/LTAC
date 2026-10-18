use anyhow::{Context, Result, bail};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    time::{Instant, timeout_at},
};

const SERVICE: &str = "urn:lge-com:service:webos-second-screen:1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub id: String,
    pub name: String,
    pub model: String,
}

impl DeviceIdentity {
    /// Keep the identity intact for matching; shorten only its display label.
    pub fn display_name(&self) -> String {
        let mut name = self.name.trim();
        for prefix in ["[LG]", "LG", "webOS", "TV"] {
            if name
                .get(..prefix.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
                && name
                    .get(prefix.len()..)
                    .is_some_and(|s| s.is_empty() || s.starts_with(char::is_whitespace))
            {
                name = name[prefix.len()..].trim_start();
            }
        }
        if name.is_empty() {
            if self.model.trim().is_empty() {
                "LG TV".into()
            } else {
                self.model.trim().into()
            }
        } else {
            name.into()
        }
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;

    #[test]
    fn short_labels_preserve_model_and_custom_names() {
        for (name, expected) in [
            ("[LG] webOS TV OLED42C4ENA", "OLED42C4ENA"),
            (" LG webOS TV OLED42C4ENA ", "OLED42C4ENA"),
            ("OLED42C4ENA", "OLED42C4ENA"),
            ("Living room", "Living room"),
            ("LGallery", "LGallery"),
            ("[LG] webOS TV", "OLED42C4ENA"),
        ] {
            let identity = DeviceIdentity {
                id: "uuid:example".into(),
                name: name.into(),
                model: "OLED42C4ENA".into(),
            };
            assert_eq!(identity.display_name(), expected);
            assert_eq!(identity.name, name);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    #[serde(flatten)]
    pub identity: DeviceIdentity,
    pub ip: Ipv4Addr,
    pub broadcast: Ipv4Addr,
}

fn unicast(ip: Ipv4Addr) -> bool {
    !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast() && !ip.is_broadcast()
}

// Only the response sender may supply its own HTTP description. Never follow
// redirects or fetch an arbitrary host from an unauthenticated SSDP packet.
fn location(packet: &str, sender: Ipv4Addr) -> Option<reqwest::Url> {
    if !packet.lines().next()?.starts_with("HTTP/1.1 200") {
        return None;
    }
    let mut st = None;
    let mut url = None;
    for line in packet.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("st") {
                st = Some(value.trim());
            }
            if name.eq_ignore_ascii_case("location") {
                url = reqwest::Url::parse(value.trim()).ok();
            }
        }
    }
    if st != Some(SERVICE) || !unicast(sender) {
        return None;
    }
    let url = url?;
    (url.scheme() == "http"
        && url.host_str()? == sender.to_string()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

fn description(xml: &str) -> Result<DeviceIdentity> {
    let doc = roxmltree::Document::parse(xml)?;
    let device = doc
        .descendants()
        .find(|n| n.has_tag_name("device"))
        .context("Missing device")?;
    let field = |name: &str| {
        device
            .children()
            .find(|n| n.has_tag_name(name))
            .and_then(|n| n.text())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let manufacturer = field("manufacturer");
    let id = field("UDN").to_ascii_lowercase();
    let name = field("friendlyName");
    let model = field("modelName");
    let supported = device
        .descendants()
        .any(|n| n.has_tag_name("serviceType") && n.text() == Some(SERVICE));
    if !manufacturer.to_ascii_lowercase().contains("lg")
        || !supported
        || !id.starts_with("uuid:")
        || id.len() < 10
        || id.len() > 128
        || name.is_empty()
        || name.len() > 256
        || model.len() > 256
    {
        bail!("Description is not an identified LG webOS TV");
    }
    Ok(DeviceIdentity { id, name, model })
}

async fn fetch(client: &reqwest::Client, url: reqwest::Url) -> Result<DeviceIdentity> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 128 * 1024 {
            bail!("Oversized TV description");
        }
        bytes.extend_from_slice(&chunk);
    }
    description(std::str::from_utf8(&bytes)?)
}

async fn search_interface(source: Ipv4Addr, broadcast: Ipv4Addr) -> Result<Vec<Device>> {
    let socket = UdpSocket::bind((source, 0)).await?;
    socket.set_multicast_ttl_v4(2)?;
    let packet = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {SERVICE}\r\n\r\n"
    );
    for _ in 0..2 {
        socket
            .send_to(packet.as_bytes(), (Ipv4Addr::new(239, 255, 255, 250), 1900))
            .await?;
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut urls = HashSet::new();
    let mut pending = Vec::new();
    let mut buffer = [0; 8192];
    while let Ok(Ok((len, sender))) = timeout_at(deadline, socket.recv_from(&mut buffer)).await {
        let IpAddr::V4(ip) = sender.ip() else {
            continue;
        };
        if let Ok(text) = std::str::from_utf8(&buffer[..len])
            && let Some(url) = location(text, ip)
            && urls.len() < 32
            && urls.insert(url.to_string())
        {
            let client = client.clone();
            pending.push(async move {
                fetch(&client, url).await.map(|identity| Device {
                    identity,
                    ip,
                    broadcast,
                })
            });
        }
    }
    Ok(join_all(pending)
        .await
        .into_iter()
        .filter_map(Result::ok)
        .collect())
}

/// A bounded, on-demand SSDP search on each active IPv4 interface. No scanner
/// or periodic discovery task remains alive after this returns.
pub async fn discover() -> Result<Vec<Device>> {
    let mut interfaces = HashSet::new();
    for interface in if_addrs::get_if_addrs()? {
        if let if_addrs::IfAddr::V4(v4) = interface.addr
            && unicast(v4.ip)
            && let Some(broadcast) = v4.broadcast
        {
            interfaces.insert((v4.ip, broadcast));
        }
    }
    let results = join_all(
        interfaces
            .into_iter()
            .map(|(ip, broadcast)| search_interface(ip, broadcast)),
    )
    .await;
    let mut devices = BTreeMap::new();
    let mut any_success = false;
    for found in results.into_iter().flatten() {
        any_success = true;
        for device in found {
            devices.entry(device.identity.id.clone()).or_insert(device);
        }
    }
    if !any_success {
        bail!("No usable local IPv4 discovery interface");
    }
    Ok(devices.into_values().collect())
}

pub fn routed_broadcast(ip: Ipv4Addr) -> Result<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((ip, 9))?;
    let source = socket.local_addr()?.ip();
    for interface in if_addrs::get_if_addrs()? {
        if let if_addrs::IfAddr::V4(v4) = interface.addr
            && IpAddr::V4(v4.ip) == source
            && let Some(broadcast) = v4.broadcast
        {
            return Ok(broadcast);
        }
    }
    bail!("No IPv4 broadcast route for selected TV")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_does_not_fetch_cross_host_urls() {
        let ip = "192.168.1.79".parse().unwrap();
        for url in [
            "http://127.0.0.1/a",
            "http://192.168.1.80/a",
            "https://192.168.1.79/a",
            "http://user@192.168.1.79/a",
        ] {
            assert!(
                location(
                    &format!("HTTP/1.1 200 OK\r\nST: {SERVICE}\r\nLOCATION: {url}\r\n"),
                    ip
                )
                .is_none()
            );
        }
        assert!(
            location(
                &format!(
                    "HTTP/1.1 200 OK\r\nST: {SERVICE}\r\nLOCATION: http://192.168.1.79:1914/\r\n"
                ),
                ip
            )
            .is_some()
        );
    }
    #[test]
    fn identity_requires_webos_service_and_stable_udn() {
        let xml = format!(
            "<root><device><manufacturer>LG Electronics</manufacturer><friendlyName>My &amp; TV</friendlyName><modelName>C2</modelName><UDN>uuid:ABCDEF</UDN><serviceList><service><serviceType>{SERVICE}</serviceType></service></serviceList></device></root>"
        );
        let identity = description(&xml).unwrap();
        assert_eq!(identity.name, "My & TV");
        assert_eq!(identity.id, "uuid:abcdef");
        assert!(description(&xml.replace(SERVICE, "other")).is_err());
        assert!(description(&xml.replace("uuid:ABCDEF", "")).is_err());
    }
}
