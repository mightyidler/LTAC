use ltac::{
    config::{Config, Transport},
    ssap::PowerState,
    wol::{MacAddress, magic_packet},
};
use serde_json::json;

#[test]
fn wol_packet_matches_network_format() {
    let mac: MacAddress = "44:27:45:51:7E:9A".parse().unwrap();
    let packet = magic_packet(mac);
    assert_eq!(packet.len(), 102);
    assert_eq!(&packet[..6], &[0xff; 6]);
    for repeat in packet[6..].chunks_exact(6) {
        assert_eq!(repeat, &[0x44, 0x27, 0x45, 0x51, 0x7e, 0x9a]);
    }
    assert_eq!("44-27-45-51-7e-9a".parse::<MacAddress>().unwrap(), mac);
    for bad in [
        "",
        "44:27:45:51:7E",
        "44:27:45:51:7E:GG",
        "FF:FF:FF:FF:FF:FF",
        "00:00:00:00:00:00",
        "4:27:45:51:7E:9A",
    ] {
        assert!(bad.parse::<MacAddress>().is_err(), "{bad}");
    }
}

#[test]
fn standby_or_screen_off_is_never_reported_as_on() {
    for raw in ["Power Off", "Suspend", "Active Standby"] {
        assert_eq!(
            PowerState::from_payload(&json!({"state":raw})),
            PowerState::Standby
        );
    }
    assert_eq!(
        PowerState::from_payload(&json!({"state":"Screen Off"})),
        PowerState::ScreenOff
    );
    assert_eq!(
        PowerState::from_payload(&json!({"state":"Active"})),
        PowerState::On
    );
    assert!(matches!(
        PowerState::from_payload(&json!({})),
        PowerState::Unknown(_)
    ));
    assert!(matches!(
        PowerState::from_payload(&json!({"state":"new-state"})),
        PowerState::Unknown(_)
    ));
}

fn config() -> Config {
    Config {
        ip: "192.168.1.79".parse().unwrap(),
        mac: Some("44:27:45:51:7E:9A".parse().unwrap()),
        broadcast: "255.255.255.255".parse().unwrap(),
        transport: Transport::Auto,
        device: None,
        source_path: None,
        wake_verified: false,
        pc_hdmi: None,
        pc_hdmi_source: Default::default(),
        protected_key: None,
        certificate_sha256: None,
    }
}

#[cfg(windows)]
#[test]
fn credential_is_protected_and_survives_atomic_config_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = config();
    config.save(&path).unwrap();
    config.set_key("test-only-secret-not-a-tv-key").unwrap();
    config.save(&path).unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(!contents.contains("test-only-secret"));
    assert_eq!(
        Config::load(&path).unwrap().key().unwrap(),
        "test-only-secret-not-a-tv-key"
    );
    config.protected_key.as_mut().unwrap()[0] ^= 0xff;
    assert!(config.key().is_err());
}

#[test]
fn invalid_config_cannot_replace_valid_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = config();
    config.save(&path).unwrap();
    config.ip = "255.255.255.255".parse().unwrap();
    assert!(config.save(&path).is_err());
    assert_eq!(Config::load(&path).unwrap().ip.to_string(), "192.168.1.79");
}

#[test]
fn legacy_config_loads_and_missing_mac_does_not_invent_a_wake_address() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut value = serde_json::to_value(config()).unwrap();
    value.as_object_mut().unwrap().remove("device");
    value.as_object_mut().unwrap().remove("wake_verified");
    value.as_object_mut().unwrap().remove("pc_hdmi");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let legacy = Config::load(&path).unwrap();
    assert!(legacy.mac.is_some());
    assert!(legacy.device.is_none());
    assert!(!legacy.wake_verified);
    assert!(legacy.pc_hdmi.is_none());
    value.as_object_mut().unwrap().remove("mac");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(Config::load(&path).unwrap().mac.is_none());
}

#[test]
fn hdmi_setup_preserves_manual_and_legacy_choices() {
    use ltac::hdmi::InputSource;
    let detection = |port| port;
    let mut auto = config();
    auto.apply_hdmi_detection(Some(detection(3)));
    assert_eq!(auto.pc_hdmi, Some(3));
    assert_eq!(auto.pc_hdmi_source, InputSource::Automatic);
    auto.apply_hdmi_detection(Some(detection(2)));
    assert_eq!(auto.pc_hdmi, Some(2));
    auto.apply_hdmi_detection(None);
    assert!(auto.pc_hdmi.is_none());
    auto.set_manual_hdmi(4);
    auto.apply_hdmi_detection(Some(detection(1)));
    auto.apply_hdmi_detection(None);
    assert_eq!(auto.pc_hdmi, Some(4));
    assert_eq!(auto.pc_hdmi_source, InputSource::Manual);
    let roundtrip: Config = serde_json::from_value(serde_json::to_value(&auto).unwrap()).unwrap();
    assert_eq!(roundtrip.pc_hdmi_source, InputSource::Manual);
    let mut legacy = config();
    legacy.pc_hdmi = Some(3);
    legacy.apply_hdmi_detection(Some(detection(1)));
    assert_eq!(legacy.pc_hdmi, Some(3));
    assert_eq!(legacy.pc_hdmi_source, InputSource::Legacy);
}
