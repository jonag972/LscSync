//! Tuya QR code login: fetches the devices of an LSC Smart Connect (or Smart
//! Life) account, with their local keys.
//!
//! This is the only part of the app that talks to the internet, and only
//! during setup. Port of the parts of Tuya's `tuya-device-sharing-sdk` (MIT)
//! that the app needs: QR login, list homes, list devices.
//!
//! Flow: [`QrLogin::start`] with the user code shown in the LSC app, display
//! [`QrLogin::qr_content`] as a QR code, poll [`QrLogin::poll`] until the user
//! confirms in the app, then [`Session::devices`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, KeyInit, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use rand::Rng;
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;

/// Client ID registered with Tuya for this login flow.
///
/// Borrowed from Home Assistant's Tuya integration, where it is public: LscSync
/// has none of its own. If Tuya ever blocks it, LscSync will need its own.
pub const CLIENT_ID: &str = "HA_3y9q4ak7g4ephrvke";
const SCHEMA: &str = "haauthorize";
const LOGIN_HOST: &str = "https://apigw.iotbing.com";
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("network: {0}")]
    Network(#[from] ureq::Error),
    #[error("Tuya refused the request: {code} {message}")]
    Refused { code: String, message: String },
    #[error("unexpected response: {0}")]
    Response(String),
}

/// A device of the account, as returned by Tuya.
#[derive(Clone, Debug, Deserialize)]
pub struct CloudDevice {
    pub id: String,
    pub name: String,
    pub local_key: String,
    #[serde(default)]
    pub product_name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub online: bool,
}

impl From<CloudDevice> for lscsync_core::Device {
    /// The LAN address is unknown here: discovery fills it in.
    fn from(device: CloudDevice) -> Self {
        Self {
            id: device.id,
            name: device.name.trim().to_owned(),
            local_key: device.local_key,
            ip: String::new(),
            product: device.product_name.trim().to_owned(),
            category: device.category,
            modes: Vec::new(),
            synced: false,
            zones: Vec::new(),
            capabilities: None,
            color_command: None,
        }
    }
}

/// A QR code waiting to be scanned.
pub struct QrLogin {
    agent: ureq::Agent,
    user_code: String,
    token: String,
}

/// An authorized session, valid for a couple of hours.
pub struct Session {
    agent: ureq::Agent,
    endpoint: String,
    access_token: String,
    refresh_token: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into()
}

/// `success: false` responses become [`CloudError::Refused`].
fn checked(response: Value) -> Result<Value, CloudError> {
    if response["success"].as_bool() == Some(true) {
        return Ok(response);
    }
    Err(CloudError::Refused {
        code: response["code"].to_string(),
        message: response["msg"].as_str().unwrap_or_default().to_owned(),
    })
}

impl QrLogin {
    /// Asks Tuya for a login token tied to the user code.
    pub fn start(user_code: &str) -> Result<Self, CloudError> {
        let agent = agent();
        let response: Value = agent
            .post(format!(
                "{LOGIN_HOST}/v1.0/m/life/home-assistant/qrcode/tokens"
            ))
            .query("clientid", CLIENT_ID)
            .query("usercode", user_code)
            .query("schema", SCHEMA)
            .send_empty()?
            .body_mut()
            .read_json()?;
        let response = checked(response)?;
        let token = response["result"]["qrcode"]
            .as_str()
            .ok_or_else(|| CloudError::Response("no QR token".to_owned()))?
            .to_owned();
        Ok(Self {
            agent,
            user_code: user_code.to_owned(),
            token,
        })
    }

    /// Text to encode in the QR code scanned by the LSC app.
    pub fn qr_content(&self) -> String {
        format!("tuyaSmart--qrLogin?token={}", self.token)
    }

    /// `Ok(None)` while the user has not confirmed in the app yet.
    pub fn poll(&self) -> Result<Option<Session>, CloudError> {
        let response: Value = self
            .agent
            .get(format!(
                "{LOGIN_HOST}/v1.0/m/life/home-assistant/qrcode/tokens/{}",
                self.token
            ))
            .query("clientid", CLIENT_ID)
            .query("usercode", &self.user_code)
            .call()?
            .body_mut()
            .read_json()?;
        if response["success"].as_bool() != Some(true) {
            return Ok(None);
        }
        let result = &response["result"];
        let field = |name: &str| {
            result[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| CloudError::Response(format!("login result without {name}")))
        };
        Ok(Some(Session {
            agent: self.agent.clone(),
            endpoint: field("endpoint")?,
            access_token: field("access_token")?,
            refresh_token: field("refresh_token")?,
        }))
    }
}

impl Session {
    /// Every device of every home of the account, with its local key.
    pub fn devices(&self) -> Result<Vec<CloudDevice>, CloudError> {
        let homes = self.get("/v1.0/m/life/users/homes", None)?;
        let homes = homes
            .as_array()
            .ok_or_else(|| CloudError::Response("homes is not a list".to_owned()))?;

        let mut devices = Vec::new();
        for home in homes {
            let home_id = match &home["ownerId"] {
                Value::String(id) => id.clone(),
                other => other.to_string(),
            };
            let list = self.get(
                "/v1.0/m/life/ha/home/devices",
                Some(serde_json::json!({ "homeId": home_id })),
            )?;
            let list: Vec<CloudDevice> = serde_json::from_value(list)
                .map_err(|error| CloudError::Response(format!("device list: {error}")))?;
            devices.extend(list);
        }
        Ok(devices)
    }

    /// What a device can do: its product key, the codes of its standard
    /// functions, and their DP ids on the local network. Tuya describes only
    /// standard functions (the panels' DP 51 is missing, for instance).
    pub fn capabilities(&self, device_id: &str) -> Result<lscsync_core::Capabilities, CloudError> {
        let specification = self.get(&format!("/v1.1/m/life/{device_id}/specifications"), None)?;
        let points = self.get(&format!("/v1.0/m/life/devices/{device_id}/status"), None)?;
        let mut functions: Vec<String> = ["functions", "status"]
            .iter()
            .flat_map(|list| specification[*list].as_array().cloned().unwrap_or_default())
            .filter_map(|function| function["code"].as_str().map(str::to_owned))
            .collect();
        functions.sort();
        functions.dedup();
        let data_points = points["dpStatusRelationDTOS"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|point| {
                Some(lscsync_core::DataPoint {
                    id: u32::try_from(point["dpId"].as_u64()?).ok()?,
                    code: point["dpCode"]
                        .as_str()
                        .or(point["statusCode"].as_str())?
                        .to_owned(),
                })
            })
            .collect();
        Ok(lscsync_core::Capabilities {
            product_key: points["productKey"].as_str().unwrap_or_default().to_owned(),
            functions,
            data_points,
        })
    }

    /// Signed, encrypted GET on the "customer" API; returns the decrypted result.
    fn get(&self, path: &str, params: Option<Value>) -> Result<Value, CloudError> {
        let request_id = random_request_id();
        let hash_key = md5_hex(&format!("{request_id}{}", self.refresh_token));
        let secret = request_secret(&request_id, &hash_key);

        let query = params
            .map(|params| encrypt(&params.to_string(), &secret, &random_nonce()))
            .unwrap_or_default();
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        let headers = [
            ("X-appKey", CLIENT_ID),
            ("X-requestId", request_id.as_str()),
            ("X-sid", ""),
            ("X-time", time.as_str()),
            ("X-token", self.access_token.as_str()),
        ];
        let sign = sign(&hash_key, &query, "", &headers);

        let mut request = self.agent.get(format!("{}{path}", self.endpoint));
        if !query.is_empty() {
            request = request.query("encdata", &query);
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response: Value = request
            .header("X-sign", &sign)
            .call()?
            .body_mut()
            .read_json()?;
        let response = checked(response)?;

        match &response["result"] {
            Value::String(encrypted) => {
                let text = decrypt(encrypted, &secret)?;
                Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
            }
            other => Ok(other.clone()),
        }
    }
}

fn random_request_id() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    let hex = hex(&bytes);
    // UUID v4 layout, as the SDK sends.
    format!(
        "{}-{}-4{}-{:x}{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        8 + (bytes[8] & 0x3),
        &hex[17..20],
        &hex[20..32]
    )
}

fn random_nonce() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTWXYZabcdefhijkmnprstwxyz2345678";
    let mut rng = rand::rng();
    (0..12)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn md5_hex(text: &str) -> String {
    hex(&Md5::digest(text.as_bytes()))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// AES key of one request: first 16 hex chars of HMAC-SHA256(request id, hash key).
fn request_secret(request_id: &str, hash_key: &str) -> String {
    hex(&hmac_sha256(request_id.as_bytes(), hash_key.as_bytes()))[..16].to_owned()
}

/// Request signature: HMAC-SHA256 over the non-empty headers, then the
/// encrypted query and body.
fn sign(hash_key: &str, query: &str, body: &str, headers: &[(&str, &str)]) -> String {
    let mut text = headers
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("||");
    text.push_str(query);
    text.push_str(body);
    hex(&hmac_sha256(hash_key.as_bytes(), text.as_bytes()))
}

/// base64(nonce) + base64(ciphertext with tag), as the SDK expects.
fn encrypt(plain: &str, secret: &str, nonce: &str) -> String {
    let cipher = Aes128Gcm::new_from_slice(secret.as_bytes()).expect("16-byte key");
    let nonce_bytes = Nonce::try_from(nonce.as_bytes()).expect("12-byte nonce");
    let ciphertext = cipher
        .encrypt(&nonce_bytes, plain.as_bytes())
        .expect("AES-GCM encryption does not fail");
    format!("{}{}", STANDARD.encode(nonce), STANDARD.encode(ciphertext))
}

/// Decrypts base64(nonce (12 bytes) + ciphertext with tag).
fn decrypt(encoded: &str, secret: &str) -> Result<String, CloudError> {
    let invalid = |what: &str| CloudError::Response(format!("cannot decrypt: {what}"));
    let data = STANDARD
        .decode(encoded)
        .map_err(|_| invalid("not base64"))?;
    if data.len() < 12 {
        return Err(invalid("too short"));
    }
    let (nonce, ciphertext) = data.split_at(12);
    let cipher = Aes128Gcm::new_from_slice(secret.as_bytes()).map_err(|_| invalid("key"))?;
    let nonce = Nonce::try_from(nonce).map_err(|_| invalid("nonce"))?;
    let plain = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| invalid("authentication failed"))?;
    String::from_utf8(plain).map_err(|_| invalid("not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values produced by Tuya's Python SDK (tuya-device-sharing-sdk).
    const REQUEST_ID: &str = "5f0c7c2e-8b1a-4d3e-9f21-0a1b2c3d4e5f";
    const HASH_KEY: &str = "f5b33d82a3df0aba8afe0f4e573360c7";
    const SECRET: &str = "3cc820b6d2b78599";

    #[test]
    fn hash_key_and_secret_match_the_sdk() {
        assert_eq!(
            md5_hex(&format!("{REQUEST_ID}refresh-token-example")),
            HASH_KEY
        );
        assert_eq!(request_secret(REQUEST_ID, HASH_KEY), SECRET);
    }

    #[test]
    fn signature_matches_the_sdk() {
        let headers = [
            ("X-appKey", CLIENT_ID),
            ("X-requestId", REQUEST_ID),
            ("X-sid", ""),
            ("X-time", "1761700000000"),
            ("X-token", "access-token-example"),
        ];
        assert_eq!(
            sign(HASH_KEY, "encrypted-query-example", "", &headers),
            "7831aaa56552d57dfe1193cbaea7a86f1bf7fe8f958057bf0ce7b2fe61febc37"
        );
    }

    #[test]
    fn query_encryption_matches_the_sdk() {
        let params = serde_json::json!({ "homeId": "12345" }).to_string();
        assert_eq!(
            encrypt(&params, SECRET, "ABCDEFGHJKMN"),
            "QUJDREVGR0hKS01OZyry8xd1YQeL1WZmovNeaPeDd4gqah/O66xSSvxLIw3+3g=="
        );
    }

    #[test]
    fn decrypts_a_server_response() {
        let server =
            "QUJDREVGR0hKS01OR3O48w1+TRHgi2ZtsvFIcfeQEeA1chz6SYreFFIeHA/l78jl4AoTzhM9MZ52UYU=";
        assert_eq!(
            decrypt(server, SECRET).unwrap(),
            r#"[{"ownerId":"1","name":"Home"}]"#
        );
        assert!(decrypt(server, "0000000000000000").is_err());
    }

    #[test]
    fn request_id_looks_like_a_uuid_v4() {
        let id = random_request_id();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!(matches!(&parts[3][..1], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn nonce_uses_the_sdk_alphabet() {
        let nonce = random_nonce();
        assert_eq!(nonce.len(), 12);
        assert!(nonce.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
