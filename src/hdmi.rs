//! Automatic HDMI setup reads only direct HDMI physical addresses from active LG EDIDs.
use serde::{Deserialize, Serialize};

/// Untagged saved numbers may be manual selections and must be preserved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    #[default]
    Legacy,
    Automatic,
    Manual,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DisplaySnapshot {
    pub displays: Vec<DisplayEdid>,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DisplayEdid {
    pub id: String,
    pub bytes: Vec<u8>,
}
impl DisplaySnapshot {
    pub fn detect(&self) -> Result<u8, &'static str> {
        if !self.complete {
            return Err("display_edid_unavailable");
        }
        let mut lg = self.displays.iter().filter(|d| is_lg(&d.bytes));
        let display = lg.next().ok_or("lg_display_unavailable")?;
        if lg.next().is_some() {
            return Err("multiple_lg_displays_select_hdmi_manually");
        }
        direct_port(&display.bytes).ok_or("direct_hdmi_address_unavailable")
    }
}
fn is_lg(edid: &[u8]) -> bool {
    matches!(edid.get(8..10), Some([0x1e, 0x6d] | [0x30, 0xe5]))
}
pub fn capture() -> DisplaySnapshot {
    active_edids()
}

fn valid_block(block: &[u8]) -> bool {
    block.len() == 128 && block.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

#[cfg(any(windows, test))]
fn apply_override(bytes: &mut Vec<u8>, index: usize, replacement: &[u8]) -> bool {
    if index >= 256 || replacement.len() != 128 {
        return false;
    }
    bytes.resize(bytes.len().max((index + 1) * 128), 0);
    bytes[index * 128..(index + 1) * 128].copy_from_slice(replacement);
    true
}

pub fn direct_port(edid: &[u8]) -> Option<u8> {
    let base = edid.get(..128)?;
    if base[..8] != [0, 255, 255, 255, 255, 255, 255, 0] || !valid_block(base) {
        return None;
    }
    // EISA manufacturer IDs used by LG TVs: GSM and LGE.
    if base[8..10] != [0x1e, 0x6d] && base[8..10] != [0x30, 0xe5] {
        return None;
    }
    let mut port = None;
    for index in 0..usize::from(base[126]) {
        let extension = edid.get((index + 1) * 128..(index + 2) * 128)?;
        if !valid_block(extension) {
            return None;
        }
        if extension[0] != 0x02 {
            continue;
        } // CTA extension
        let end = usize::from(extension[2]);
        if end == 0 {
            continue;
        }
        if !(4..=127).contains(&end) {
            return None;
        }
        let mut cursor = 4;
        while cursor < end {
            let header = extension[cursor];
            let length = usize::from(header & 0x1f);
            let next = cursor + 1 + length;
            if next > end {
                return None;
            }
            let data = &extension[cursor + 1..next];
            // HDMI vendor-specific data block, little-endian IEEE OUI 0x000c03.
            if header >> 5 == 3 && length >= 5 && data[..3] == [3, 12, 0] {
                let address = u16::from_be_bytes([data[3], data[4]]);
                let candidate = (address >> 12) as u8;
                // Only a direct TV connection is unambiguous. Receivers and
                // switches may rewrite or emulate downstream EDIDs.
                if !(1..=4).contains(&candidate) || address & 0x0fff != 0 || port.is_some() {
                    return None;
                }
                port = Some(candidate);
            }
            cursor = next;
        }
    }
    port
}

#[cfg(not(windows))]
fn active_edids() -> DisplaySnapshot {
    DisplaySnapshot::default()
}

#[cfg(windows)]
fn active_edids() -> DisplaySnapshot {
    use std::{
        mem::size_of,
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Graphics::Gdi::{
            DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW,
            EnumDisplayDevicesW,
        },
        System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RegGetValueW},
    };
    fn device() -> DISPLAY_DEVICEW {
        DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        }
    }
    fn text(value: &[u16]) -> String {
        String::from_utf16_lossy(
            &value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())],
        )
    }
    let mut snapshot = DisplaySnapshot {
        displays: Vec::new(),
        complete: true,
    };
    let mut seen = std::collections::HashSet::new();
    // Read only monitors attached to this session, never stale registry entries.
    // All Win32 buffers are owned and live for their synchronous calls.
    unsafe {
        for index in 0..=64 {
            let mut adapter = device();
            if EnumDisplayDevicesW(null(), index, &mut adapter, 0) == 0 {
                break;
            }
            if index == 64 {
                snapshot.complete = false;
                break;
            }
            if adapter.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0 {
                continue;
            }
            for monitor_index in 0..=64 {
                let mut monitor = device();
                if EnumDisplayDevicesW(adapter.DeviceName.as_ptr(), monitor_index, &mut monitor, 1)
                    == 0
                {
                    break;
                }
                if monitor_index == 64 {
                    snapshot.complete = false;
                    break;
                }
                if monitor.StateFlags & DISPLAY_DEVICE_ACTIVE == 0 {
                    continue;
                }
                let interface = text(&monitor.DeviceID);
                let parts = interface.split('#').collect::<Vec<_>>();
                if parts.len() != 4 || !parts[0].eq_ignore_ascii_case(r"\\?\DISPLAY") {
                    snapshot.complete = false;
                    continue;
                }
                if parts[1..3]
                    .iter()
                    .any(|part| part.is_empty() || part.contains(['\\', '/']))
                {
                    snapshot.complete = false;
                    continue;
                }
                if !seen.insert(interface.clone()) {
                    continue;
                }
                let path = format!(
                    r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{}\{}\Device Parameters",
                    parts[1], parts[2]
                );
                let path = path.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
                let name = "EDID\0".encode_utf16().collect::<Vec<_>>();
                let mut bytes = vec![0u8; 32768];
                let mut size = bytes.len() as u32;
                if RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    path.as_ptr(),
                    name.as_ptr(),
                    RRF_RT_REG_BINARY,
                    null_mut(),
                    bytes.as_mut_ptr().cast(),
                    &mut size,
                ) == 0
                {
                    bytes.truncate(size as usize);
                    // Overrides are per 128-byte block and take precedence.
                    let override_path =
                        String::from_utf16_lossy(&path[..path.len() - 1]) + r"\EDID_OVERRIDE";
                    let override_path = override_path
                        .encode_utf16()
                        .chain(Some(0))
                        .collect::<Vec<_>>();
                    for block in 0..256usize {
                        let name = block
                            .to_string()
                            .encode_utf16()
                            .chain(Some(0))
                            .collect::<Vec<_>>();
                        let mut replacement = [0u8; 128];
                        let mut size = 128;
                        let status = RegGetValueW(
                            HKEY_LOCAL_MACHINE,
                            override_path.as_ptr(),
                            name.as_ptr(),
                            RRF_RT_REG_BINARY,
                            null_mut(),
                            replacement.as_mut_ptr().cast(),
                            &mut size,
                        );
                        if status == 0 {
                            if size != 128 || !apply_override(&mut bytes, block, &replacement) {
                                snapshot.complete = false;
                            }
                        } else if status != 2 && status != 3 {
                            snapshot.complete = false;
                        }
                    }
                    snapshot.displays.push(DisplayEdid {
                        id: interface,
                        bytes,
                    });
                } else {
                    snapshot.complete = false;
                }
            }
        }
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edid_direct_address_selects_hdmi_three_and_rejects_ambiguous_paths() {
        let mut bytes = vec![0u8; 256];
        bytes[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        bytes[8..10].copy_from_slice(&[0x1e, 0x6d]);
        bytes[126] = 1;
        bytes[128..138].copy_from_slice(&[2, 3, 10, 0, 0x65, 3, 12, 0, 0x30, 0]);
        fn checksum(block: &mut [u8]) {
            block[127] = 0u8.wrapping_sub(block[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b)));
        }
        checksum(&mut bytes[..128]);
        checksum(&mut bytes[128..]);
        assert_eq!(direct_port(&bytes), Some(3));
        let display = DisplayEdid {
            id: "LG".into(),
            bytes: bytes.clone(),
        };
        let mut snapshot = DisplaySnapshot {
            displays: vec![display.clone()],
            complete: true,
        };
        assert_eq!(snapshot.detect(), Ok(3));
        snapshot.displays.push(display);
        assert!(snapshot.detect().is_err());
        bytes[137] = 0x10;
        checksum(&mut bytes[128..]);
        assert_eq!(direct_port(&bytes), None);
        assert_eq!(direct_port(&[]), None);
    }
}
