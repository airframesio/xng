//! Airframes ATC feeder sign-in (`xng atc login`) and token refresh.
//!
//! Sign-in uses the api-server's OAuth-style endpoints with client id
//! `airframes-feeder` and scope `atc:feed`: the device flow (approve on any
//! browser) or email + password. Only the rotating refresh token is stored,
//! with the feeder id, in `~/.config/xng/atc.json` (mode 0600). Access tokens
//! live an hour and are minted from the refresh token before each connect.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CLIENT_ID: &str = "airframes-feeder";
pub const SCOPE: &str = "atc:feed";
pub const DEFAULT_API: &str = "https://api.airframes.io";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// What `atc.json` holds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    /// api-server base URL the tokens belong to.
    pub api: Option<String>,
    pub refresh_token: Option<String>,
    pub username: Option<String>,
    pub user_id: Option<String>,
    /// Stable feeder identity (UUID); one per installation.
    pub feeder_id: Option<String>,
}

/// A usable access token.
#[derive(Clone, Debug)]
pub struct AccessToken {
    pub token: String,
    /// Unix seconds; `None` when unknown (fixed tokens).
    pub expires_at: Option<u64>,
}

impl AccessToken {
    /// Seconds until expiry (saturating), or `None` for fixed tokens.
    pub fn remaining_s(&self) -> Option<u64> {
        self.expires_at.map(|e| e.saturating_sub(now_s()))
    }
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `$XDG_CONFIG_HOME/xng` (or `~/.config/xng`; `%APPDATA%\xng` on Windows).
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("xng");
    }
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("APPDATA") {
        return PathBuf::from(d).join("xng");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("xng")
}

pub fn default_path() -> PathBuf {
    config_dir().join("atc.json")
}

pub fn load(path: &Path) -> anyhow::Result<Credentials> {
    match std::fs::read(path) {
        Ok(b) => Ok(serde_json::from_slice(&b).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
        Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
    }
}

/// Write the credentials file, readable by the owner only.
pub fn save(path: &Path, c: &Credentials) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp)?;
        std::io::Write::write_all(&mut f, &serde_json::to_vec_pretty(c)?)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The feeder id in `path`, created (and saved) on first use.
pub fn feeder_id(path: &Path) -> anyhow::Result<String> {
    let mut c = load(path)?;
    if let Some(id) = c.feeder_id.as_ref().filter(|s| uuid::Uuid::parse_str(s).is_ok()) {
        return Ok(id.clone());
    }
    let id = uuid::Uuid::new_v4().to_string();
    c.feeder_id = Some(id.clone());
    save(path, &c)?;
    Ok(id)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(alias = "accessToken", alias = "token")]
    access_token: Option<String>,
    #[serde(alias = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(alias = "expiresIn")]
    expires_in: Option<u64>,
    user: Option<serde_json::Value>,
    error: Option<String>,
    error_description: Option<String>,
    message: Option<serde_json::Value>,
}

fn post(url: &str, body: serde_json::Value) -> anyhow::Result<(u16, serde_json::Value)> {
    let resp = ureq::post(url)
        .timeout(Duration::from_secs(20))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .set("User-Agent", concat!("xng/", env!("CARGO_PKG_VERSION")))
        .send_string(&body.to_string());
    let (status, text) = match resp {
        Ok(r) => (r.status(), r.into_string()?),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(e) => return Err(e.into()),
    };
    let v = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "message": text }));
    Ok((status, v))
}

fn api_error(status: u16, v: &serde_json::Value) -> anyhow::Error {
    let msg = v
        .get("error_description")
        .or_else(|| v.get("message"))
        .or_else(|| v.get("error"))
        .map(|m| m.as_str().map(str::to_string).unwrap_or_else(|| m.to_string()))
        .unwrap_or_default();
    anyhow::anyhow!("Airframes API returned {status}: {msg}")
}

/// Turn a token response into an access token, updating `creds`.
fn absorb(creds: &mut Credentials, t: TokenResponse) -> anyhow::Result<AccessToken> {
    let token = t.access_token.ok_or_else(|| anyhow::anyhow!("token response has no access_token"))?;
    if let Some(r) = t.refresh_token {
        creds.refresh_token = Some(r);
    }
    if let Some(u) = &t.user {
        if let Some(n) = u.get("username").and_then(|v| v.as_str()) {
            creds.username = Some(n.to_string());
        }
        if let Some(id) = u.get("id") {
            creds.user_id = Some(id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string()));
        }
    }
    let expires_at = t.expires_in.map(|s| now_s() + s).or_else(|| jwt_exp(&token));
    Ok(AccessToken { token, expires_at })
}

/// `exp` from a JWT's payload, without verifying it.
pub fn jwt_exp(token: &str) -> Option<u64> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes).ok()?.get("exp")?.as_u64()
}

/// Mint an access token from the stored refresh token (rotating it).
/// Blocking: call from `spawn_blocking`.
pub fn refresh(path: &Path) -> anyhow::Result<AccessToken> {
    let mut c = load(path)?;
    let rt = c
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("not signed in to the Airframes ATC network; run `xng atc login`"))?;
    let api = c.api.clone().unwrap_or_else(|| DEFAULT_API.to_string());
    let (status, v) = post(
        &format!("{}/auth/token", api.trim_end_matches('/')),
        serde_json::json!({ "grant_type": "refresh_token", "refresh_token": rt, "client_id": CLIENT_ID }),
    )?;
    if status >= 400 {
        if status == 400 || status == 401 {
            return Err(anyhow::anyhow!(
                "{}; the saved sign-in was rejected, run `xng atc login` again",
                api_error(status, &v)
            ));
        }
        return Err(api_error(status, &v));
    }
    let t: TokenResponse = serde_json::from_value(v)?;
    let at = absorb(&mut c, t)?;
    save(path, &c)?;
    Ok(at)
}

/// Email + password sign-in. Blocking.
pub fn login_password(path: &Path, api: &str, email: &str, password: &str) -> anyhow::Result<Credentials> {
    let (status, v) = post(
        &format!("{}/auth/login", api.trim_end_matches('/')),
        serde_json::json!({ "email": email, "password": password, "client_id": CLIENT_ID }),
    )?;
    if status >= 400 {
        return Err(api_error(status, &v));
    }
    finish_login(path, api, serde_json::from_value(v)?)
}

/// What the user must do to approve a device sign-in.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

fn default_interval() -> u64 {
    5
}

/// Start the device flow. Blocking.
pub fn device_start(api: &str) -> anyhow::Result<DeviceCode> {
    let (status, v) = post(
        &format!("{}/auth/device/code", api.trim_end_matches('/')),
        serde_json::json!({ "client_id": CLIENT_ID, "scope": SCOPE }),
    )?;
    if status >= 400 {
        return Err(api_error(status, &v));
    }
    Ok(serde_json::from_value(v)?)
}

/// Poll until the user approves (or the code expires). Blocking.
pub fn device_wait(path: &Path, api: &str, dc: &DeviceCode) -> anyhow::Result<Credentials> {
    let mut interval = dc.interval.max(1);
    let deadline = now_s() + dc.expires_in.unwrap_or(900);
    loop {
        std::thread::sleep(Duration::from_secs(interval));
        anyhow::ensure!(now_s() < deadline, "the sign-in code expired; run `xng atc login` again");
        let (status, v) = post(
            &format!("{}/auth/token", api.trim_end_matches('/')),
            serde_json::json!({ "grant_type": DEVICE_GRANT, "device_code": dc.device_code, "client_id": CLIENT_ID }),
        )?;
        if status < 400 {
            return finish_login(path, api, serde_json::from_value(v)?);
        }
        match v.get("error").and_then(|e| e.as_str()) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("access_denied") => anyhow::bail!("the sign-in was declined"),
            Some("expired_token") => anyhow::bail!("the sign-in code expired; run `xng atc login` again"),
            _ => return Err(api_error(status, &v)),
        }
    }
}

fn finish_login(path: &Path, api: &str, t: TokenResponse) -> anyhow::Result<Credentials> {
    if let Some(e) = &t.error {
        anyhow::bail!("{e}: {}", t.error_description.clone().unwrap_or_default());
    }
    let _ = &t.message;
    let mut c = load(path)?;
    c.api = (api != DEFAULT_API).then(|| api.to_string());
    let had_refresh = t.refresh_token.is_some();
    absorb(&mut c, t)?;
    anyhow::ensure!(
        had_refresh,
        "the Airframes API did not return a refresh token for the feeder client; nothing was saved"
    );
    if c.feeder_id.is_none() {
        c.feeder_id = Some(uuid::Uuid::new_v4().to_string());
    }
    save(path, &c)?;
    Ok(c)
}

/// Forget the sign-in (the feeder id is kept so feeds keep their identity).
pub fn logout(path: &Path) -> anyhow::Result<bool> {
    let mut c = load(path)?;
    let was = c.refresh_token.take().is_some();
    c.username = None;
    c.user_id = None;
    save(path, &c)?;
    Ok(was)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_round_trip_private() {
        let dir = std::env::temp_dir().join(format!("xng-atc-{}", uuid::Uuid::new_v4()));
        let p = dir.join("atc.json");
        let id = feeder_id(&p).unwrap();
        assert_eq!(feeder_id(&p).unwrap(), id, "feeder id is stable");
        let mut c = load(&p).unwrap();
        c.refresh_token = Some("r1".into());
        save(&p, &c).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(logout(&p).unwrap());
        let c = load(&p).unwrap();
        assert!(c.refresh_token.is_none());
        assert_eq!(c.feeder_id.as_deref(), Some(id.as_str()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_jwt_expiry() {
        use base64::Engine as _;
        let b = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        let t = format!("{}.{}.sig", b("{\"alg\":\"EdDSA\"}"), b("{\"sub\":\"1\",\"exp\":1800000000}"));
        assert_eq!(jwt_exp(&t), Some(1_800_000_000));
        assert_eq!(jwt_exp("dev:1"), None);
    }
}
