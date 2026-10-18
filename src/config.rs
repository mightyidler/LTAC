use std::{
    fs,
    io::Write,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use crate::wol::MacAddress;
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Auto,
    Wss,
    Ws,
}

// Deliberately no Debug: this structure contains a protected credential.
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
    pub ip: Ipv4Addr,
    #[serde(default)]
    pub mac: Option<MacAddress>,
    #[serde(default)]
    pub device: Option<crate::discovery::DeviceIdentity>,
    #[serde(default)]
    pub wake_verified: bool,
    pub broadcast: Ipv4Addr,
    pub transport: Transport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected_key: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_sha256: Option<String>,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.ip.is_unspecified() || self.ip.is_multicast() || self.ip.is_broadcast() {
            bail!("TV IP must be an IPv4 unicast address");
        }
        if self.broadcast.is_unspecified() || self.broadcast.is_multicast() {
            bail!("Invalid Wake-on-LAN destination");
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self = serde_json::from_slice(
            &fs::read(path)
                .with_context(|| format!("Read {}. Run `ltac setup` first.", path.display()))?,
        )?;
        config.validate()?;
        config.source_path = Some(path.to_owned());
        Ok(config)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .context("Config requires a parent directory")?;
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&serde_json::to_vec_pretty(self)?)?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .context("Atomically save configuration")?;
        Ok(())
    }
    pub fn key(&self) -> Result<String> {
        let data = self
            .protected_key
            .as_ref()
            .context("Not paired. Turn the TV on manually and run `ltac pair`.")?;
        let key = String::from_utf8(unprotect(data)?).context("Invalid stored key")?;
        if key.is_empty() {
            bail!("Empty client key. Run `ltac pair --reset`.");
        }
        Ok(key)
    }
    pub fn set_key(&mut self, key: &str) -> Result<()> {
        if key.is_empty() {
            bail!("TV returned an empty client key");
        }
        self.protected_key = Some(protect(key.as_bytes())?);
        Ok(())
    }

    /// Only export to an administrator-protected service directory. Machine
    /// DPAPI permits the LocalService account to decrypt before user logon.
    #[cfg(windows)]
    pub fn for_service(&self) -> Result<Self> {
        self.validate()?;
        let mut exported = self.clone();
        exported.source_path = None;
        exported.protected_key = Some(crypt_with_scope(self.key()?.as_bytes(), true, true)?);
        Ok(exported)
    }
}

pub fn default_path() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("No user data directory")?
        .data_local_dir()
        .join("LTAC")
        .join("config.json"))
}

#[cfg(windows)]
fn crypt(data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    crypt_with_scope(data, encrypt, false)
}

#[cfg(windows)]
fn crypt_with_scope(data: &[u8], encrypt: bool, machine: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN,
            CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len().try_into()?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // DPAPI allocates output with LocalAlloc. Input lives for the entire call;
    // output is copied before LocalFree, on both encrypt and decrypt paths.
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN
                    | if machine {
                        CRYPTPROTECT_LOCAL_MACHINE
                    } else {
                        0
                    },
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("Windows DPAPI credential protection");
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Ok(result)
    }
}
#[cfg(windows)]
fn protect(data: &[u8]) -> Result<Vec<u8>> {
    crypt(data, true)
}
#[cfg(windows)]
fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    crypt(data, false)
}
#[cfg(not(windows))]
fn protect(_: &[u8]) -> Result<Vec<u8>> {
    bail!("Credential storage currently requires Windows DPAPI")
}
#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Vec<u8>> {
    bail!("Credential storage currently requires Windows DPAPI")
}
