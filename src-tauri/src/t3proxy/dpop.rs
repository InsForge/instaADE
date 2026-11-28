//! InstaADE's own (t3boat talks to T3 with a bearer session): the proxy's DPoP session on T3
//! (RFC 9449), as T3 0.1.11 and later run it.
//!
//! - Each sandbox has its own P-256 key, kept with its session in the keychain. A key is the
//!   identity T3 binds the session to, so one per sandbox keeps a sandbox's session its own.
//! - Pairing: a one-time credential from `t3 auth pairing create` (run through the exec API),
//!   exchanged at `/oauth/token` with a proof. T3 answers with a `DPoP` access token for one
//!   hour, renewable on the same session for 30 days after the last renewal.
//! - Every request and every WebSocket upgrade carries a fresh proof: `htm`, `htu` (the URL
//!   without query or fragment, `wss` and `ws` as `https` and `http`), `iat`, a new `jti`, and
//!   `ath` with the access token.
//! - Nothing here logs a token, a credential or the key.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

/// `subject_token_type` of a pairing credential.
pub const BOOTSTRAP_TOKEN_TYPE: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
/// `subject_token_type` of an access token that renews its own session.
pub const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

/// Issues a one-time pairing credential for the proxy on the sandbox, as JSON on stdout. The
/// base directory is the `t3-instabox` service's (see `t3::PAIR_SCRIPT`).
pub const PAIRING_SCRIPT: &str = r#"t3 auth pairing create --base-dir "$HOME/.t3-instabox/$(cat /etc/machine-id)" --json --ttl 5m --label instaade-proxy"#;

/// The credential `PAIRING_SCRIPT` printed.
pub fn parse_pairing_credential(stdout: &str) -> Option<String> {
    let start = stdout.find('{')?;
    let v: Value = serde_json::from_str(stdout[start..].trim()).ok()?;
    v["credential"]
        .as_str()
        .filter(|c| !c.trim().is_empty())
        .map(str::to_owned)
}

fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

fn sha256_b64(text: &str) -> String {
    b64(ring::digest::digest(&ring::digest::SHA256, text.as_bytes()).as_ref())
}

/// `ath`: the access token's SHA-256, base64url.
pub fn ath(access_token: &str) -> String {
    sha256_b64(access_token)
}

/// `htu`: `url` without its query and fragment. A WebSocket URL names the HTTP request the
/// server sees, so `wss` reads as `https` and `ws` as `http`.
pub fn htu(url: &str) -> Result<String> {
    let mut u = url::Url::parse(url).context("DPoP URL is invalid")?;
    let scheme = match u.scheme() {
        "wss" => "https",
        "ws" => "http",
        other => other,
    }
    .to_owned();
    if scheme != u.scheme() {
        // `set_scheme` refuses a switch between special schemes it treats alike; rebuild.
        let rest = &url[url.find("://").context("DPoP URL is invalid")?..];
        u = url::Url::parse(&format!("{scheme}{rest}")).context("DPoP URL is invalid")?;
    }
    u.set_query(None);
    u.set_fragment(None);
    Ok(u.to_string())
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The proxy's P-256 proof key for one sandbox.
pub struct DpopKey {
    pair: EcdsaKeyPair,
    pkcs8: String,
    jwk: Value,
    thumbprint: String,
}

impl std::fmt::Debug for DpopKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DpopKey({})", self.thumbprint)
    }
}

impl DpopKey {
    pub fn generate() -> Result<Self> {
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| anyhow::anyhow!("could not make a DPoP key"))?;
        Self::from_pkcs8(&b64(doc.as_ref()))
    }

    /// The key as it is stored: its PKCS#8 document, base64url.
    pub fn from_pkcs8(stored: &str) -> Result<Self> {
        let der = B64
            .decode(stored)
            .context("the stored DPoP key is malformed")?;
        let pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &SystemRandom::new())
                .map_err(|_| anyhow::anyhow!("the stored DPoP key is malformed"))?;
        // An uncompressed point: 0x04, then X and Y, 32 bytes each.
        let point = pair.public_key().as_ref();
        if point.len() != 65 || point[0] != 4 {
            bail!("unexpected DPoP public key");
        }
        let (x, y) = (b64(&point[1..33]), b64(&point[33..]));
        // RFC 7638: the required members in lexicographic order, as T3 computes it.
        let thumbprint = sha256_b64(&format!(
            r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#
        ));
        let jwk = json!({"kty": "EC", "crv": "P-256", "x": x, "y": y});
        Ok(DpopKey {
            pair,
            pkcs8: stored.to_owned(),
            jwk,
            thumbprint,
        })
    }

    pub fn pkcs8(&self) -> &str {
        &self.pkcs8
    }

    /// The public JWK, as proofs carry it.
    pub fn jwk(&self) -> &Value {
        &self.jwk
    }

    /// The RFC 7638 thumbprint T3 binds the session to.
    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }

    /// A fresh proof for `method` on `url`, bound to `access_token` when one is given.
    pub fn proof(&self, method: &str, url: &str, access_token: Option<&str>) -> Result<String> {
        self.proof_at(method, url, access_token, now_secs())
    }

    fn proof_at(
        &self,
        method: &str,
        url: &str,
        access_token: Option<&str>,
        iat: i64,
    ) -> Result<String> {
        let header = json!({"typ": "dpop+jwt", "alg": "ES256", "jwk": self.jwk});
        let mut payload = json!({
            "htm": method.to_ascii_uppercase(),
            "htu": htu(url)?,
            "jti": uuid::Uuid::new_v4().to_string(),
            "iat": iat,
        });
        if let Some(token) = access_token {
            payload["ath"] = Value::String(ath(token));
        }
        let input = format!(
            "{}.{}",
            b64(header.to_string().as_bytes()),
            b64(payload.to_string().as_bytes())
        );
        let signature = self
            .pair
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|_| anyhow::anyhow!("could not sign a DPoP proof"))?;
        Ok(format!("{input}.{}", b64(signature.as_ref())))
    }
}

/// The proxy's upstream credential for one sandbox, as kept in the keychain. `Debug` shows no
/// secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum Credential {
    /// A long-lived bearer session (`t3 auth session issue`), for a T3 without DPoP. Only with
    /// `INSTAADE_T3_PROXY=any`.
    Bearer { token: String },
    /// A DPoP-bound session: the key (PKCS#8, base64url), the access token and when it expires
    /// (unix ms).
    Dpop {
        key: String,
        token: String,
        expires_at: i64,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Bearer { .. } => write!(f, "Credential::Bearer(..)"),
            Credential::Dpop { expires_at, .. } => {
                write!(f, "Credential::Dpop(.., expires_at: {expires_at})")
            }
        }
    }
}

impl Credential {
    /// What the keychain holds. The first prototype kept a bare bearer token there.
    pub fn parse(stored: &str) -> Credential {
        serde_json::from_str(stored).unwrap_or_else(|_| Credential::Bearer {
            token: stored.to_owned(),
        })
    }

    pub fn to_stored(&self) -> String {
        serde_json::to_string(self).expect("a credential serializes")
    }

    pub fn is_dpop(&self) -> bool {
        matches!(self, Credential::Dpop { .. })
    }
}

/// True when a token expiring at `expires_at` (unix ms) should be renewed at `now_ms`: it has
/// `before` or less left.
pub fn renewal_due(expires_at: i64, now_ms: i64, before: Duration) -> bool {
    expires_at - now_ms <= before.as_millis() as i64
}

/// T3 refused the proxy's session, and pairing again did not help. Never answered with a
/// bearer token instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "T3 on the sandbox refused the proxy's DPoP session ({}). Use Open in T3 Code to pair it again.",
            self.0
        )
    }
}

impl std::error::Error for Refused {}

/// Trades `subject` (a pairing credential, or the current access token to renew it) for a DPoP
/// access token bound to `key`. Returns the token and when it expires (unix ms). A `Bearer`
/// answer is an error: the session must be bound to the key.
pub async fn exchange(
    http: &reqwest::Client,
    base_url: &str,
    key: &DpopKey,
    subject: &str,
    subject_type: &str,
) -> Result<(String, i64)> {
    let url = format!("{}/oauth/token", base_url.trim_end_matches('/'));
    let proof = key.proof("POST", &url, None)?;
    let res = http
        .post(&url)
        .header("dpop", proof)
        .form(&[
            ("grant_type", TOKEN_EXCHANGE),
            ("subject_token", subject),
            ("subject_token_type", subject_type),
            ("requested_token_type", ACCESS_TOKEN_TYPE),
            ("client_label", "InstaADE proxy"),
        ])
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    let status = res.status();
    if !status.is_success() {
        let body: Value = res.json().await.unwrap_or_default();
        let reason = body["dpopFailureReason"]
            .as_str()
            .or(body["code"].as_str())
            .or(body["error"].as_str())
            .unwrap_or("refused");
        bail!("token exchange: HTTP {status} ({reason})");
    }
    let body: Value = res
        .json()
        .await
        .context("token exchange: malformed answer")?;
    if body["token_type"] != "DPoP" {
        bail!("token exchange: T3 did not bind the session to the key");
    }
    let token = body["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .context("token exchange: no access token")?
        .to_owned();
    let expires_in = body["expires_in"].as_i64().unwrap_or(3600).max(0);
    Ok((
        token,
        chrono::Utc::now().timestamp_millis() + expires_in * 1000,
    ))
}

/// True when T3 at `base_url` takes DPoP sessions: its auth descriptor, which
/// `/api/auth/session` returns to anyone, lists `dpop-access-token`.
pub async fn supports_dpop(http: &reqwest::Client, base_url: &str) -> Result<bool> {
    let v: Value = http
        .get(format!(
            "{}/api/auth/session",
            base_url.trim_end_matches('/')
        ))
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .json()
        .await
        .context("T3 sent a malformed session state")?;
    Ok(v["auth"]["sessionMethods"]
        .as_array()
        .is_some_and(|m| m.iter().any(|x| x == "dpop-access-token")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Checks `proof` as T3's verifier does; returns its header and payload.
    pub(crate) fn verify(proof: &str) -> (Value, Value) {
        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        let payload: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        let x = B64.decode(header["jwk"]["x"].as_str().unwrap()).unwrap();
        let y = B64.decode(header["jwk"]["y"].as_str().unwrap()).unwrap();
        let mut point = vec![4u8];
        point.extend(x);
        point.extend(y);
        let signature = B64.decode(parts[2]).unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, point)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .expect("the signature verifies with the public JWK");
        (header, payload)
    }

    #[test]
    fn a_proof_carries_the_claims_t3_checks_and_verifies_with_its_jwk() {
        let key = DpopKey::generate().unwrap();
        let proof = key
            .proof("get", "https://3774-a.example/api/x?y=1#z", Some("tok"))
            .unwrap();
        let (header, payload) = verify(&proof);
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(&header["jwk"], key.jwk());
        assert!(header["jwk"].get("d").is_none(), "no private part");
        assert_eq!(payload["htm"], "GET");
        assert_eq!(payload["htu"], "https://3774-a.example/api/x");
        assert_eq!(payload["ath"], ath("tok"));
        assert_eq!(
            payload["ath"], "GnZ0607njffhrEOak8P6jjyUV4TU3sn9jjARc4svHWI",
            "base64url SHA-256 of `tok`"
        );
        assert!((payload["iat"].as_i64().unwrap() - now_secs()).abs() <= 2);
        assert!(uuid::Uuid::parse_str(payload["jti"].as_str().unwrap()).is_ok());
        let (_, again) = verify(
            &key.proof("GET", "https://3774-a.example/api/x", Some("tok"))
                .unwrap(),
        );
        assert_ne!(again["jti"], payload["jti"], "a new jti every time");
        // Without a token, no `ath` (the token exchange).
        let (_, exchange) = verify(&key.proof("POST", "https://h/oauth/token", None).unwrap());
        assert!(exchange.get("ath").is_none());
        assert_eq!(exchange["htm"], "POST");
    }

    #[test]
    fn a_websocket_proof_names_the_http_request() {
        assert_eq!(
            htu("wss://3774-a.example/ws?wsTicket=t&dpop=p").unwrap(),
            "https://3774-a.example/ws"
        );
        assert_eq!(
            htu("ws://127.0.0.1:9/ws?x").unwrap(),
            "http://127.0.0.1:9/ws"
        );
        assert_eq!(htu("https://h").unwrap(), "https://h/");
        assert_eq!(
            htu("https://h/api/orchestration/threads/a%2Fb/bounded?deferWork=1").unwrap(),
            "https://h/api/orchestration/threads/a%2Fb/bounded"
        );
        assert!(htu("not a url").is_err());
    }

    #[test]
    fn the_thumbprint_is_rfc_7638() {
        // RFC 7638's own example is RSA; this checks the member order T3 hashes for EC keys.
        let key = DpopKey::generate().unwrap();
        let jwk = key.jwk();
        let input = format!(
            r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
            jwk["x"].as_str().unwrap(),
            jwk["y"].as_str().unwrap()
        );
        assert_eq!(key.thumbprint(), sha256_b64(&input));
        assert_eq!(B64.decode(jwk["x"].as_str().unwrap()).unwrap().len(), 32);
    }

    #[test]
    fn a_stored_key_signs_as_before() {
        let key = DpopKey::generate().unwrap();
        let again = DpopKey::from_pkcs8(key.pkcs8()).unwrap();
        assert_eq!(again.thumbprint(), key.thumbprint());
        verify(&again.proof("GET", "https://h/x", None).unwrap());
        assert!(DpopKey::from_pkcs8("not-a-key").is_err());
        assert!(!format!("{key:?}").contains(key.pkcs8()));
    }

    #[test]
    fn renewal_is_due_ten_minutes_before_expiry() {
        let before = Duration::from_secs(600);
        let now = 1_000_000_000;
        assert!(!renewal_due(now + 3_600_000, now, before));
        assert!(!renewal_due(now + 600_001, now, before));
        assert!(renewal_due(now + 600_000, now, before));
        assert!(
            renewal_due(now - 1, now, before),
            "expired: renewed, T3 takes it"
        );
    }

    #[test]
    fn credentials_round_trip_and_show_no_secret() {
        let dpop = Credential::Dpop {
            key: "k".into(),
            token: "secret-token".into(),
            expires_at: 5,
        };
        assert_eq!(Credential::parse(&dpop.to_stored()), dpop);
        assert!(!format!("{dpop:?}").contains("secret-token"));
        // The first prototype kept a bare bearer token.
        assert_eq!(
            Credential::parse("abcdefghijklmnop"),
            Credential::Bearer {
                token: "abcdefghijklmnop".into()
            }
        );
        assert!(!format!("{:?}", Credential::parse("abcdefghijklmnop")).contains("abcdef"));
    }

    #[test]
    fn the_pairing_credential_is_read_from_the_json() {
        let out = "{\n  \"id\": \"p1\",\n  \"credential\": \"one-time\",\n  \"scopes\": []\n}\n";
        assert_eq!(parse_pairing_credential(out).as_deref(), Some("one-time"));
        assert_eq!(
            parse_pairing_credential(&format!("note: x\n{out}")).as_deref(),
            Some("one-time")
        );
        assert_eq!(parse_pairing_credential("Token: x"), None);
        assert_eq!(parse_pairing_credential("{\"credential\": \"\"}"), None);
        assert!(
            PAIRING_SCRIPT.contains(r#"--base-dir "$HOME/.t3-instabox/$(cat /etc/machine-id)""#)
        );
        assert!(PAIRING_SCRIPT.contains("--json"));
    }

    #[test]
    fn refusals_say_what_to_do() {
        let e = Refused("token exchange: HTTP 401".into()).to_string();
        assert!(e.contains("Open in T3 Code"));
        assert!(!e.contains("Bearer"));
    }
}
