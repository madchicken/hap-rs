use base64::Engine;
use ed25519_dalek::SigningKey as Ed25519Keypair;
use log::debug;
//use eui48::MacAddress;
use macaddr::MacAddr6 as MacAddress;
use rand::{Rng, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::net::Ipv4Addr;

use crate::{BonjourFeatureFlag, BonjourStatusFlag, Pin, accessory::AccessoryCategory};

/// The `Config` struct is used to store configuration options for the HomeKit Accessory Server.
///
/// # Examples
///
/// ```
/// use hap::{accessory::AccessoryCategory, Config, MacAddress, Pin};
///
/// let config = Config {
///     pin: Pin::new([1, 1, 1, 2, 2, 3, 3, 3]).unwrap(),
///     name: "Acme Lightbulb".into(),
///     device_id: MacAddress::from([10, 20, 30, 40, 50, 60]),
///     category: AccessoryCategory::Lightbulb,
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    /// Socket IP address to serve on. Defaults to the IP of the system's first non-loopback network interface.
    pub host: Ipv4Addr,
    /// Port to serve on. Defaults to `32000`.
    pub port: u16,
    /// 8 digit pin used for pairing. Defaults to `11122333`.
    ///
    /// The following pins are considered too easy and are therefore not allowed:
    /// - `12345678`
    /// - `87654321`
    /// - `00000000`
    /// - `11111111`
    /// - `22222222`
    /// - `33333333`
    /// - `44444444`
    /// - `55555555`
    /// - `66666666`
    /// - `77777777`
    /// - `88888888`
    /// - `99999999`
    pub pin: Pin,
    /// Model name of the accessory. E.g. "Acme Lightbulb".
    pub name: String,
    /// Device ID of the accessory. Generated randomly if not specified. This value is also used as the accessory's
    /// Pairing Identifier. Must be a unique random number generated at every factory reset and must persist across
    /// reboots.
    pub device_id: MacAddress, // Bonjour: id
    pub device_ed25519_keypair: Ed25519Keypair,
    /// Current configuration number. Is updated when an accessory, service, or characteristic is added or removed on
    /// the accessory server. Accessories must increment the config number after a firmware update.
    pub configuration_number: u64, // Bonjour: c#
    /// Current state number. This must have a value of `1`.
    pub state_number: u8, // Bonjour: s#
    /// Accessory category. Indicates the category that best describes the primary function of the accessory.
    pub category: AccessoryCategory, // Bonjour: ci
    /// Protocol version string `<major>.<minor>` (e.g. `"1.0"`). Defaults to `"1.0"` Required if value is not `"1.0"`.
    pub protocol_version: String, // Bonjour: pv
    /// Bonjour Status Flag. Defaults to `StatusFlag::NotPaired` and is changed to `StatusFlag::Zero` after a
    /// successful pairing.
    pub status_flag: BonjourStatusFlag, // Bonjour: sf
    /// Bonjour Feature Flag. Currently only used to indicate MFi compliance.
    pub feature_flag: BonjourFeatureFlag, // Bonjour: ff
    /// Optional maximum number of paired controllers.
    pub max_peers: Option<usize>,
    /// Optional setup ID.
    pub setup_id: Option<String>,
}

impl Config {
    /// Redetermines the `host` field to the IP of the system's first non-loopback network interface.
    pub fn redetermine_local_ip(&mut self) {
        self.host = get_local_ip();
    }

    /// Derives mDNS TXT records from the `Config`.
    pub(crate) fn txt_records(&self) -> Vec<(String, String)> {
        vec![
            ("c#".to_string(), self.configuration_number.to_string()),
            ("ff".to_string(), (self.feature_flag as u8).to_string()),
            ("id".to_string(), self.device_id.to_string()),
            ("md".to_string(), self.name.to_string()),
            ("pv".to_string(), self.protocol_version.to_string()),
            ("s#".to_string(), self.state_number.to_string()),
            ("sf".to_string(), (self.status_flag as u8).to_string()),
            ("ci".to_string(), (self.category as u8).to_string()),
            (
                "sh".to_string(),
                compute_sh(&self.setup_id.clone().unwrap_or_default(), &self.device_id.to_string()),
            ),
        ]
    }

    pub fn setup_url(&self) -> String {
        generate_setup_url(
            &self.pin.to_string(),
            self.category as u8,
            &self.setup_id.clone().unwrap_or_default(),
        )
    }
}

impl Default for Config {
    fn default() -> Config {
        Config {
            host: get_local_ip(),
            port: 32000,
            pin: Pin::new([1, 1, 1, 2, 2, 3, 3, 3]).unwrap(),
            name: "Accessory".into(),
            device_id: generate_random_mac_address(),
            device_ed25519_keypair: generate_ed25519_keypair(),
            configuration_number: 1,
            state_number: 1,
            category: AccessoryCategory::Other,
            protocol_version: "1.1".into(),
            status_flag: BonjourStatusFlag::NotPaired,
            feature_flag: BonjourFeatureFlag::Zero,
            max_peers: None,
            setup_id: Some(generate_setup_id()),
        }
    }
}

/// Generates a random MAC address.
fn generate_random_mac_address() -> MacAddress {
    let mut csprng = OsRng {};
    let eui = csprng.gen::<[u8; 6]>();
    MacAddress::from(eui)
}

/// Generates an Ed25519 keypair.
fn generate_ed25519_keypair() -> Ed25519Keypair {
    let mut csprng = OsRng {};
    Ed25519Keypair::generate(&mut csprng)
}

/// Returns the IP of the system's first non-loopback network interface or defaults to `127.0.0.1`.
fn get_local_ip() -> Ipv4Addr {
    for iface in if_addrs::get_if_addrs().unwrap() {
        if iface.is_loopback() {
            continue;
        }

        if let std::net::IpAddr::V4(ipv4) = iface.ip() {
            return ipv4;
        }
    }
    "127.0.0.1".parse().unwrap()
}

fn generate_setup_url(pincode: &str, category: u8, setup_id: &str) -> String {
    // Rimuove i '-' e converte in numero
    let value_low_str = pincode.replace('-', "");
    let value_low = value_low_str.parse::<u64>().unwrap_or(0);

    let version = 0;
    let reserved = 0;
    let flag = 2;
    let mut payload: u64 = 0;

    payload |= version & 0x7;
    payload <<= 4;
    payload |= reserved & 0xf;

    payload <<= 8;
    payload |= category as u64 & 0xff;

    payload <<= 4;
    payload |= flag & 0xf;
    payload <<= 27u64;
    payload |= value_low & 0x07ff_ffff;

    // Converte in base36 e uppercase
    let mut encoded_payload = base36_encode(payload).to_uppercase();

    // Padding a 9 caratteri
    while encoded_payload.len() < 9 {
        encoded_payload.insert(0, '0');
    }

    format!("X-HM://{encoded_payload}{setup_id}")
}

fn base36_encode(mut num: u64) -> String {
    let mut chars = Vec::new();
    while num > 0 {
        let rem = (num % 36) as u8;
        chars.push(if rem < 10 {
            (b'0' + rem) as char
        } else {
            (b'A' + rem - 10) as char
        });
        num /= 36;
    }
    chars.reverse();
    if chars.is_empty() {
        chars.push('0');
    }
    chars.into_iter().collect()
}

pub fn compute_sh(setup_id: &str, accessory_id: &str) -> String {
    // SetupID + AccessoryID
    let input = format!("{}{}", setup_id, accessory_id.to_uppercase());
    debug!("Calculating sh for input {input}");
    // 1. SHA-512
    let hash = Sha512::digest(input.as_bytes());
    // 2. Primi 4 byte
    let first4 = &hash[..4];
    // 3. Base64 standard (url-safe)
    base64::prelude::BASE64_URL_SAFE.encode(first4)
}

fn generate_setup_id() -> String {
    let mut rng = rand::thread_rng();
    let chars = [
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V',
        'W', 'X', 'Y', 'Z', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
    ];
    let mut id = String::new();
    for _ in 0..4 {
        id.push(chars[rng.gen_range(0..chars.len())]);
    }
    id
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_generate_setup_uri() {
        let pincode = "841-31-633";
        let category = 8; // Switch
        let setup_id = "";
        let url = super::generate_setup_url(pincode, category, setup_id);
        assert_eq!(url, "X-HM://0081YCYEP");
    }

    #[test]
    fn test_generate_setup_uri_with_setup_id() {
        let pincode = "841-31-633";
        let category = 8; // Switch
        let setup_id = "3QYT";
        let uri = super::generate_setup_url(pincode, category, setup_id);
        assert_eq!(uri, "X-HM://0081YCYEP3QYT");
    }

    #[test]
    fn test_compute_sh() {
        let setup_id = "XYZK";
        let accessory_id = "00:25:29:17:01:EC";

        let sh = super::compute_sh(setup_id, accessory_id);
        assert_eq!("d_fBuw==", sh); // deve stampare: d/fB
    }
}
