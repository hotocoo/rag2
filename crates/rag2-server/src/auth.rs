//! Authentication (API keys, OIDC bearer tokens) and authorization (roles -> permissions, tenant
//! scope). Built from the active profile; rebuilt on every activation.

use arc_swap::ArcSwap;
use axum::http::HeaderMap;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use parking_lot::Mutex;
use rag2_config::schema::{ApiKey, AuthConfig, Permission, Profile};
use rag2_core::{Error, Result, TenantId};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// Minimum spacing between JWKS refetches triggered by unknown key ids (stops a flood of forged
/// tokens from turning into a flood of requests to the identity provider).
const JWKS_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(30);
const JWKS_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
pub const API_KEY_HEADER: &str = "x-api-key";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantScope {
    All,
    Only(BTreeSet<String>),
}

/// Who is calling, and what they may do.
#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub permissions: BTreeSet<Permission>,
    pub tenants: TenantScope,
}

impl Principal {
    pub fn require(&self, p: Permission) -> Result<()> {
        if self.permissions.contains(&p) {
            Ok(())
        } else {
            Err(Error::Forbidden(format!(
                "{} lacks permission {p:?}",
                self.id
            )))
        }
    }

    pub fn require_tenant(&self, p: Permission, t: &TenantId) -> Result<()> {
        self.require(p)?;
        match &self.tenants {
            TenantScope::All => Ok(()),
            TenantScope::Only(s) if s.contains(t.as_str()) => Ok(()),
            // Same message whether the tenant exists or not: no tenant enumeration.
            TenantScope::Only(_) => Err(Error::Forbidden(format!(
                "{} may not access tenant {t}",
                self.id
            ))),
        }
    }
}

fn scope(tenants: &[String]) -> TenantScope {
    if tenants.iter().any(|t| t == "*") {
        TenantScope::All
    } else {
        TenantScope::Only(tenants.iter().cloned().collect())
    }
}

fn permissions(p: &Profile, roles: &[String]) -> BTreeSet<Permission> {
    roles
        .iter()
        .filter_map(|r| p.security.roles.get(r))
        .flat_map(|r| r.permissions.iter().copied())
        .collect()
}

struct ResolvedKey {
    hash: [u8; 32],
    principal: Principal,
}

struct Oidc {
    issuer: String,
    audience: String,
    jwks_url: String,
    roles_claim: String,
    tenants_claim: String,
    keys: ArcSwap<JwkSet>,
    last_fetch: Mutex<Option<Instant>>,
    http: reqwest::Client,
}

pub struct Authenticator {
    mode: Mode,
    keys: Vec<ResolvedKey>,
    /// Roles -> permissions, for OIDC principals.
    profile: Arc<Profile>,
}

enum Mode {
    Open,
    Keys,
    Oidc(Box<Oidc>),
}

fn resolve_keys(p: &Profile, keys: &[ApiKey]) -> Result<Vec<ResolvedKey>> {
    keys.iter()
        .map(|k| {
            let secret = k
                .secret
                .resolve()
                .map_err(|e| Error::InvalidInput(format!("api key {}: {e}", k.id)))?;
            if secret.len() < MIN_API_KEY_LEN {
                return Err(Error::InvalidInput(format!(
                    "api key {} is shorter than {MIN_API_KEY_LEN} characters",
                    k.id
                )));
            }
            Ok(ResolvedKey {
                hash: *blake3::hash(secret.as_bytes()).as_bytes(),
                principal: Principal {
                    id: format!("key:{}", k.id),
                    permissions: permissions(p, &k.roles),
                    tenants: scope(&k.tenants),
                },
            })
        })
        .collect()
}

/// Shorter keys are refused at activation: they are guessable.
pub const MIN_API_KEY_LEN: usize = 24;

impl Authenticator {
    pub fn from_profile(p: Arc<Profile>) -> Result<Self> {
        let (mode, keys) = match &p.security.auth {
            AuthConfig::None => (Mode::Open, Vec::new()),
            AuthConfig::ApiKeys { keys } => (Mode::Keys, resolve_keys(&p, keys)?),
            AuthConfig::Oidc {
                issuer,
                audience,
                jwks_url,
                roles_claim,
                tenants_claim,
                api_keys,
            } => {
                let http = reqwest::Client::builder()
                    .timeout(JWKS_FETCH_TIMEOUT)
                    .build()
                    .map_err(Error::internal)?;
                (
                    Mode::Oidc(Box::new(Oidc {
                        issuer: issuer.clone(),
                        audience: audience.clone(),
                        jwks_url: jwks_url.clone(),
                        roles_claim: roles_claim.clone(),
                        tenants_claim: tenants_claim.clone(),
                        keys: ArcSwap::from_pointee(JwkSet { keys: Vec::new() }),
                        last_fetch: Mutex::new(None),
                        http,
                    })),
                    resolve_keys(&p, api_keys)?,
                )
            }
        };
        Ok(Authenticator {
            mode,
            keys,
            profile: p,
        })
    }

    pub fn is_open(&self) -> bool {
        matches!(self.mode, Mode::Open)
    }

    fn by_key(&self, presented: &str) -> Option<Principal> {
        let h = blake3::hash(presented.as_bytes());
        // Check every key so timing does not reveal which one (or whether any) matched early.
        let mut found = None;
        for k in &self.keys {
            if bool::from(k.hash.ct_eq(h.as_bytes())) {
                found = Some(k.principal.clone());
            }
        }
        found
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Principal> {
        if let Mode::Open = self.mode {
            return Ok(Principal {
                id: "anonymous".into(),
                permissions: self
                    .profile
                    .security
                    .roles
                    .values()
                    .flat_map(|r| r.permissions.iter().copied())
                    .collect(),
                tenants: TenantScope::All,
            });
        }
        let bearer = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim);
        let api_key = headers
            .get(API_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim);
        if let Some(k) = api_key.or(bearer)
            && let Some(p) = self.by_key(k)
        {
            return Ok(p);
        }
        if let (Mode::Oidc(o), Some(token)) = (&self.mode, bearer)
            && token.matches('.').count() == 2
        {
            return self.verify_jwt(o, token).await;
        }
        Err(Error::Unauthenticated(
            "missing or invalid credentials".into(),
        ))
    }

    async fn verify_jwt(&self, o: &Oidc, token: &str) -> Result<Principal> {
        let header =
            decode_header(token).map_err(|_| Error::Unauthenticated("malformed token".into()))?;
        let kid = header
            .kid
            .ok_or_else(|| Error::Unauthenticated("token has no key id".into()))?;
        let mut jwk = o.keys.load().find(&kid).cloned();
        if jwk.is_none() {
            o.refresh().await?;
            jwk = o.keys.load().find(&kid).cloned();
        }
        let jwk =
            jwk.ok_or_else(|| Error::Unauthenticated("token signed by an unknown key".into()))?;
        let key = DecodingKey::from_jwk(&jwk)
            .map_err(|_| Error::Unauthenticated("unusable signing key".into()))?;
        let alg = jwk
            .common
            .key_algorithm
            .and_then(|a| a.to_string().parse().ok())
            .unwrap_or(header.alg);
        // The token must use the algorithm the key is for: no algorithm confusion (e.g. an HMAC
        // token "signed" with a public RSA key as the secret).
        if header.alg != alg || !key_allows(&jwk.algorithm, alg) {
            return Err(Error::Unauthenticated(
                "token algorithm does not match its key".into(),
            ));
        }
        let mut v = Validation::new(alg);
        v.set_issuer(&[&o.issuer]);
        v.set_audience(&[&o.audience]);
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let data = decode::<HashMap<String, serde_json::Value>>(token, &key, &v)
            .map_err(|e| Error::Unauthenticated(format!("token rejected: {e}")))?;
        let claims = data.claims;
        let strings = |name: &str| -> Vec<String> {
            match claims.get(name) {
                Some(serde_json::Value::Array(a)) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect(),
                Some(serde_json::Value::String(s)) => {
                    s.split_whitespace().map(String::from).collect()
                }
                _ => Vec::new(),
            }
        };
        let sub = claims
            .get("sub")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");
        Ok(Principal {
            id: format!("oidc:{sub}"),
            permissions: permissions(&self.profile, &strings(&o.roles_claim)),
            tenants: scope(&strings(&o.tenants_claim)),
        })
    }
}

fn key_allows(
    params: &jsonwebtoken::jwk::AlgorithmParameters,
    alg: jsonwebtoken::Algorithm,
) -> bool {
    use jsonwebtoken::Algorithm::*;
    use jsonwebtoken::jwk::AlgorithmParameters as P;
    match params {
        P::RSA(_) => matches!(alg, RS256 | RS384 | RS512 | PS256 | PS384 | PS512),
        P::EllipticCurve(_) => matches!(alg, ES256 | ES384),
        P::OctetKeyPair(_) => matches!(alg, EdDSA),
        P::OctetKey(_) => matches!(alg, HS256 | HS384 | HS512),
        // Key types added in future versions are refused until mapped here.
        _ => false,
    }
}

impl Oidc {
    async fn refresh(&self) -> Result<()> {
        {
            let mut last = self.last_fetch.lock();
            if last.is_some_and(|t| t.elapsed() < JWKS_REFRESH_MIN_INTERVAL) {
                return Ok(());
            }
            *last = Some(Instant::now());
        }
        let set: JwkSet = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| Error::Unavailable(format!("fetching JWKS: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Unavailable(format!("parsing JWKS: {e}")))?;
        self.keys.store(Arc::new(set));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rag2_config::schema::SecretRef;

    fn profile_with_key(secret_env: &str, roles: &[&str], tenants: &[&str]) -> Arc<Profile> {
        let mut p = Profile::minimal("t");
        p.security.auth = AuthConfig::ApiKeys {
            keys: vec![ApiKey {
                id: "ci".into(),
                secret: SecretRef::Env(secret_env.into()),
                roles: roles.iter().map(|s| s.to_string()).collect(),
                tenants: tenants.iter().map(|s| s.to_string()).collect(),
            }],
        };
        Arc::new(p)
    }

    fn headers(k: &str, v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).expect("name"),
            v.parse().expect("value"),
        );
        h
    }

    #[tokio::test]
    async fn api_keys_authenticate_and_scope_permissions_and_tenants() {
        let secret = "k".repeat(MIN_API_KEY_LEN);
        unsafe_env("RAG2_TEST_KEY_A", &secret);
        let a = Authenticator::from_profile(profile_with_key(
            "RAG2_TEST_KEY_A",
            &["reader"],
            &["acme"],
        ))
        .expect("auth");
        let p = a
            .authenticate(&headers("authorization", &format!("Bearer {secret}")))
            .await
            .expect("principal");
        let acme = TenantId::new("acme").expect("t");
        let other = TenantId::new("other").expect("t");
        assert!(p.require_tenant(Permission::Query, &acme).is_ok());
        assert!(p.require_tenant(Permission::Query, &other).is_err());
        assert!(p.require_tenant(Permission::Ingest, &acme).is_err());
        assert!(
            a.authenticate(&headers(API_KEY_HEADER, &secret))
                .await
                .is_ok()
        );
        assert!(
            a.authenticate(&headers(API_KEY_HEADER, "wrong-key-wrong-key-wrong"))
                .await
                .is_err()
        );
        assert!(a.authenticate(&HeaderMap::new()).await.is_err());
    }

    #[test]
    fn short_or_missing_keys_are_refused_at_activation() {
        unsafe_env("RAG2_TEST_KEY_SHORT", "short");
        assert!(
            Authenticator::from_profile(profile_with_key(
                "RAG2_TEST_KEY_SHORT",
                &["reader"],
                &["*"]
            ))
            .is_err()
        );
        assert!(
            Authenticator::from_profile(profile_with_key(
                "RAG2_TEST_KEY_UNSET_XYZ",
                &["reader"],
                &["*"]
            ))
            .is_err()
        );
    }

    fn unsafe_env(k: &str, v: &str) {
        // Edition 2024 marks set_var unsafe because it races other threads reading the
        // environment; these keys are unique per test and only read by the same test.
        #[allow(unsafe_code)]
        // SAFETY: the variable name is unique to one test; no other thread reads it concurrently.
        unsafe {
            std::env::set_var(k, v)
        };
    }

    mod oidc {
        use super::*;
        use axum::Json;
        use jsonwebtoken::{EncodingKey, Header, encode};

        const SECRET: &[u8] = b"test-oidc-shared-secret-0123456789";

        async fn jwks_server(kid: &str) -> String {
            let k = base64_url(SECRET);
            let set = serde_json::json!({ "keys": [{ "kty": "oct", "kid": kid, "alg": "HS256", "k": k }] });
            let app = axum::Router::new().route(
                "/jwks",
                axum::routing::get(move || async move { Json(set) }),
            );
            let l = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = l.local_addr().expect("addr");
            tokio::spawn(async move { axum::serve(l, app).await });
            format!("http://{addr}/jwks")
        }

        fn base64_url(b: &[u8]) -> String {
            const A: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in b.chunks(3) {
                let n = (u32::from(c[0]) << 16)
                    | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                    | u32::from(*c.get(2).unwrap_or(&0));
                for i in 0..=c.len() {
                    out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
                }
            }
            out
        }

        fn authenticator(jwks: String) -> Authenticator {
            let mut p = Profile::minimal("t");
            p.security.auth = AuthConfig::Oidc {
                issuer: "https://idp.example".into(),
                audience: "rag2".into(),
                jwks_url: jwks,
                roles_claim: "roles".into(),
                tenants_claim: "tenants".into(),
                api_keys: vec![],
            };
            Authenticator::from_profile(Arc::new(p)).expect("auth")
        }

        fn token(kid: &str, alg: jsonwebtoken::Algorithm, claims: serde_json::Value) -> String {
            let mut h = Header::new(alg);
            h.kid = Some(kid.into());
            encode(&h, &claims, &EncodingKey::from_secret(SECRET)).expect("encode")
        }

        fn claims(exp_offset: i64, aud: &str) -> serde_json::Value {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_secs() as i64;
            serde_json::json!({ "sub": "alice", "iss": "https://idp.example", "aud": aud, "exp": now + exp_offset,
                                "roles": ["reader"], "tenants": ["acme"] })
        }

        fn bearer(t: &str) -> HeaderMap {
            headers("authorization", &format!("Bearer {t}"))
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn valid_tokens_map_roles_and_tenants_and_bad_ones_are_refused() {
            let a = authenticator(jwks_server("k1").await);
            let ok = a
                .authenticate(&bearer(&token(
                    "k1",
                    jsonwebtoken::Algorithm::HS256,
                    claims(300, "rag2"),
                )))
                .await
                .expect("valid token");
            assert_eq!(ok.id, "oidc:alice");
            assert!(
                ok.require_tenant(Permission::Query, &TenantId::new("acme").expect("t"))
                    .is_ok()
            );
            assert!(
                ok.require_tenant(Permission::Query, &TenantId::new("globex").expect("t"))
                    .is_err()
            );
            assert!(ok.require(Permission::Ingest).is_err());
            for (name, t) in [
                (
                    "expired",
                    token("k1", jsonwebtoken::Algorithm::HS256, claims(-300, "rag2")),
                ),
                (
                    "wrong audience",
                    token("k1", jsonwebtoken::Algorithm::HS256, claims(300, "other")),
                ),
                (
                    "unknown key",
                    token("k9", jsonwebtoken::Algorithm::HS256, claims(300, "rag2")),
                ),
                (
                    "algorithm mismatch",
                    token("k1", jsonwebtoken::Algorithm::HS512, claims(300, "rag2")),
                ),
            ] {
                assert!(
                    a.authenticate(&bearer(&t)).await.is_err(),
                    "{name} accepted"
                );
            }
            let mut forged = token("k1", jsonwebtoken::Algorithm::HS256, claims(300, "rag2"));
            forged.push('x');
            assert!(
                a.authenticate(&bearer(&forged)).await.is_err(),
                "bad signature accepted"
            );
        }
    }
}
