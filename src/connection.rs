use crate::{
    config::{Config, Transport},
    discovery::{self, Device},
    journal::Journal,
    ssap::{PowerState, Session},
    wol::MacAddress,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{net::Ipv4Addr, path::Path};

fn network_mac(info: &Value, ip: Ipv4Addr) -> Option<MacAddress> {
    // Use only the interface reporting the exact selected TV address. A TV
    // may expose both Ethernet and Wi-Fi MACs; the other one cannot wake it.
    for name in ["wired", "wifi", "wiredInfo", "wifiInfo"] {
        let interface = &info[name];
        if interface["ipAddress"].as_str() == Some(ip.to_string().as_str())
            && interface["state"].as_str() == Some("connected")
        {
            return interface["macAddress"].as_str()?.parse().ok();
        }
    }
    None
}

#[cfg(windows)]
fn neighbor_mac(ip: Ipv4Addr) -> Result<Option<MacAddress>> {
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::{FreeMibTable, GetIpNetTable2},
        Networking::WinSock::{AF_INET, NlnsIncomplete, NlnsUnreachable},
    };
    let mut table = std::ptr::null_mut();
    // The OS allocates this variable-length table. Read exactly NumEntries
    // rows and release with FreeMibTable. No elevation or external process.
    unsafe {
        let code = GetIpNetTable2(AF_INET, &mut table);
        if code != 0 {
            return Err(std::io::Error::from_raw_os_error(code as i32).into());
        }
        struct Guard(*mut std::ffi::c_void);
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe { FreeMibTable(self.0) };
            }
        }
        let _guard = Guard(table.cast());
        let mut found = Vec::new();
        for row in std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
        {
            let address = row.Address.Ipv4.sin_addr.S_un.S_addr;
            if address.to_ne_bytes() == ip.octets()
                && row.PhysicalAddressLength == 6
                && row.State != NlnsUnreachable
                && row.State != NlnsIncomplete
            {
                let mac = MacAddress(row.PhysicalAddress[..6].try_into()?);
                if mac.to_string().parse::<MacAddress>().is_ok() {
                    found.push(mac);
                }
            }
        }
        found.dedup();
        // Conflicting neighbors across interfaces are ambiguous; never guess.
        Ok((found.len() == 1).then(|| found[0]))
    }
}
#[cfg(not(windows))]
fn neighbor_mac(_: Ipv4Addr) -> Result<Option<MacAddress>> {
    Ok(None)
}

pub async fn detect_mac(
    session: &mut Session,
    ip: Ipv4Addr,
    journal: &mut Journal,
) -> Result<Option<MacAddress>> {
    match session
        .request("ssap://com.webos.service.connectionmanager/getinfo")
        .await
    {
        Ok(info) => {
            if let Some(mac) = network_mac(&info, ip) {
                journal.record("mac_detected", json!({"source":"tv_network", "mac":mac}))?;
                return Ok(Some(mac));
            }
            journal.record("tv_network_mac_unavailable", json!({"fields":info.as_object().map(|v|v.keys().collect::<Vec<_>>()), "wired_fields":info["wiredInfo"].as_object().map(|v|v.keys().collect::<Vec<_>>()), "wifi_fields":info["wifiInfo"].as_object().map(|v|v.keys().collect::<Vec<_>>())}))?;
        }
        Err(error) => journal.record(
            "tv_network_info_unavailable",
            json!({"error":error.to_string()}),
        )?,
    }
    match neighbor_mac(ip) {
        Ok(mac) => {
            journal.record(
                "mac_detection",
                json!({"source":"windows_neighbor", "mac":mac, "found":mac.is_some()}),
            )?;
            Ok(mac)
        }
        Err(error) => {
            journal.record("mac_detection_failed", json!({"error":error.to_string()}))?;
            Ok(None)
        }
    }
}

/// Selection is a Device obtained from SSDP, never a user supplied MAC/IP pair.
/// Existing credentials are reused only with the same saved stable identity.
pub async fn pair_device(
    device: &Device,
    path: &Path,
    reset: bool,
    journal: &mut Journal,
) -> Result<Config> {
    let old = Config::load(path).ok().filter(|c| {
        c.device
            .as_ref()
            .is_some_and(|d| d.id == device.identity.id)
    });
    let mut config = Config {
        source_path: Some(path.to_owned()),
        ip: device.ip,
        mac: None,
        device: Some(device.identity.clone()),
        broadcast: device.broadcast,
        transport: Transport::Auto,
        wake_verified: false,
        protected_key: None,
        certificate_sha256: None,
    };
    if !reset && let Some(old) = old {
        config.protected_key = old.protected_key;
        config.certificate_sha256 = old.certificate_sha256;
        config.transport = old.transport;
        config.mac = old.mac;
        config.wake_verified = old.wake_verified;
    }
    pair_and_save(config, path, journal).await
}

pub async fn pair_manual(ip: Ipv4Addr, path: &Path, journal: &mut Journal) -> Result<Config> {
    let config = Config {
        source_path: Some(path.to_owned()),
        ip,
        mac: None,
        device: None,
        broadcast: discovery::routed_broadcast(ip)?,
        transport: Transport::Auto,
        wake_verified: false,
        protected_key: None,
        certificate_sha256: None,
    };
    config.validate()?;
    pair_and_save(config, path, journal).await
}

async fn pair_and_save(mut config: Config, path: &Path, journal: &mut Journal) -> Result<Config> {
    let key = if config.protected_key.is_some() {
        Some(config.key()?)
    } else {
        None
    };
    let mut session = Session::connect(
        config.ip,
        config.transport,
        config.certificate_sha256.as_deref(),
    )
    .await?;
    journal.record(
        "pairing_wait",
        json!({"name":config.device.as_ref().map(|d| &d.name), "reusing_key":key.is_some()}),
    )?;
    let key = session.register(key.as_deref(), true).await?;
    // A registered frame alone is not enough to claim a working connection.
    let power = session.power_state().await?;
    anyhow::ensure!(
        !matches!(power, PowerState::Unknown(_)),
        "TV did not provide a supported power state"
    );
    let detected = detect_mac(&mut session, config.ip, journal).await?;
    if detected != config.mac {
        config.wake_verified = false;
    }
    config.mac = detected;
    config.set_key(&key)?;
    config.certificate_sha256 = session.certificate_sha256.clone();
    config.save(path)?;
    journal.record("connection_saved", json!({"device":config.device, "ip":config.ip, "mac_available":config.mac.is_some(), "state":power, "wake_verified":config.wake_verified}))?;
    Ok(config)
}

/// Resolve a changed address by stable UDN. Pairing key and TLS pin must still
/// authenticate the discovered host before any persisted address is replaced.
pub async fn recover(config: &Config) -> Result<(Config, Session)> {
    let identity = config
        .device
        .as_ref()
        .context("No saved discovery identity; automatic address recovery unavailable")?;
    let device = discovery::discover()
        .await?
        .into_iter()
        .find(|d| d.identity.id == identity.id)
        .context("Saved TV was not found on the local network")?;
    let mut candidate = config.clone();
    candidate.ip = device.ip;
    candidate.broadcast = device.broadcast;
    let mut session = Session::connect(
        candidate.ip,
        candidate.transport,
        candidate.certificate_sha256.as_deref(),
    )
    .await?;
    session.register(Some(&candidate.key()?), false).await?;
    session.power_state().await?;
    let mac = session
        .request("ssap://com.webos.service.connectionmanager/getinfo")
        .await
        .ok()
        .and_then(|info| network_mac(&info, candidate.ip))
        .or_else(|| neighbor_mac(candidate.ip).ok().flatten());
    if mac != candidate.mac {
        candidate.wake_verified = false;
    }
    candidate.mac = mac;
    if let Some(path) = &candidate.source_path {
        candidate.save(path)?;
    }
    Ok((candidate, session))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_must_belong_to_connected_selected_interface() {
        let ip = "192.168.1.79".parse().unwrap();
        let info = json!({"wired":{"state":"connected","ipAddress":"192.168.1.79","macAddress":"44:27:45:51:7E:9A"},"wifi":{"state":"disconnected","ipAddress":"","macAddress":"44:27:45:51:7E:9B"}});
        assert_eq!(
            network_mac(&info, ip).unwrap().to_string(),
            "44:27:45:51:7E:9A"
        );
        assert!(network_mac(&info, "192.168.1.80".parse().unwrap()).is_none());
        assert!(network_mac(&json!({"wired":{"macAddress":"44:27:45:51:7E:9A"}}), ip).is_none());
    }
}
