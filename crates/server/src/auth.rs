//! Who a request comes from: the Operator, by a trusted proxy's identity header or by the Operator token, else no one.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::StartError;
use crate::routes::error_response;

/// The identity a request was authenticated as, recorded as the actor of the Jobs it starts.
#[derive(Debug, Clone)]
pub struct Actor(pub String);

/// The actor recorded for a request authenticated by the Operator token.
pub const TOKEN_ACTOR: &str = "Operator token";

/// The flag naming the request header that carries the Operator's identity, and its environment variable.
pub const IDENTITY_HEADER: (&str, &str) = ("--identity-header", "TITLE_SELECT_IDENTITY_HEADER");
/// The flag listing the proxies whose identity header is trusted, and its environment variable.
pub const TRUSTED_PROXIES: (&str, &str) = ("--trusted-proxies", "TITLE_SELECT_TRUSTED_PROXIES");
/// The flag giving the development identity, and its environment variable.
pub const DEV_IDENTITY: (&str, &str) = ("--dev-identity", "TITLE_SELECT_DEV_IDENTITY");
/// The flag naming the file that holds the Operator token, and its environment variable.
pub const TOKEN_FILE: (&str, &str) = ("--token-file", "TITLE_SELECT_OPERATOR_TOKEN_FILE");

/// How the Service knows a request comes from the Operator, as `serve`'s flags and their environment variables give
/// it (ADR 0029). The Operator token, read from [`Settings::token_file`](crate::Settings::token_file), is the other
/// way. Kept outside the store, so the workbench can't change who reaches it.
#[derive(Debug, Clone, Default)]
pub struct AuthSettings {
    /// The request header carrying the Operator's identity, set by an authenticating reverse proxy.
    pub identity_header: Option<String>,
    /// The only peers whose `identity_header` is trusted.
    pub trusted_proxies: Vec<IpNet>,
    /// An identity given to every request with no other credential, for development. Only allowed while the
    /// Service listens on a loopback address.
    pub dev_identity: Option<String>,
}

/// A flag as the Operator sees it: `--flag` (`ENV`).
fn flag((flag, env): (&str, &str)) -> String {
    format!("{flag} ({env})")
}

#[derive(Debug)]
pub struct Auth {
    identity_header: Option<HeaderName>,
    trusted_proxies: Vec<IpNet>,
    /// Only the token's SHA-256 is kept.
    token_sha256: Option<[u8; 32]>,
    dev_identity: Option<String>,
}

impl Auth {
    /// Refuses a Service with no way to authenticate the Operator, an identity header without trusted proxies or
    /// the other way round, an identity header that isn't a header name, or an empty dev identity or one off
    /// loopback.
    pub fn new(
        settings: &AuthSettings,
        token_file: Option<&Path>,
        listen: SocketAddr,
    ) -> Result<Auth, StartError> {
        let refuse = |why: String| Err(StartError::Auth(why));
        let identity_header = match (
            &settings.identity_header,
            settings.trusted_proxies.is_empty(),
        ) {
            (Some(header), false) => Some(HeaderName::try_from(header.as_str()).map_err(|_| {
                StartError::Auth(format!(
                    "{} {header:?} is not a header name",
                    flag(IDENTITY_HEADER)
                ))
            })?),
            (Some(_), true) => {
                return refuse(format!(
                    "{} needs {}: the proxies whose identity header you trust",
                    flag(IDENTITY_HEADER),
                    flag(TRUSTED_PROXIES)
                ));
            }
            (None, false) => {
                return refuse(format!(
                    "{} needs {}: the header those proxies name you in",
                    flag(TRUSTED_PROXIES),
                    flag(IDENTITY_HEADER)
                ));
            }
            (None, true) => None,
        };
        if settings
            .dev_identity
            .as_deref()
            .is_some_and(|i| i.trim().is_empty())
        {
            return refuse(format!("{} is empty", flag(DEV_IDENTITY)));
        }
        let token_sha256 = token_file.map(read_token).transpose()?;
        if identity_header.is_none() && token_sha256.is_none() && settings.dev_identity.is_none() {
            return refuse(format!(
                "no way to authenticate requests: set {} with {}, or {}, or {} on a loopback address",
                flag(IDENTITY_HEADER),
                flag(TRUSTED_PROXIES),
                flag(TOKEN_FILE),
                flag(DEV_IDENTITY)
            ));
        }
        if settings.dev_identity.is_some() && !listen.ip().is_loopback() {
            return refuse(format!(
                "{} is only allowed on a loopback listen address, not {listen}",
                flag(DEV_IDENTITY)
            ));
        }
        Ok(Auth {
            identity_header,
            trusted_proxies: settings.trusted_proxies.clone(),
            token_sha256,
            dev_identity: settings.dev_identity.clone(),
        })
    }

    fn authenticate(&self, peer: IpAddr, headers: &HeaderMap) -> Result<Actor, &'static str> {
        if let Some(name) = &self.identity_header
            && let Some(value) = headers.get(name)
        {
            let peer = peer.to_canonical();
            if !self.trusted_proxies.iter().any(|net| net.contains(&peer)) {
                return Err("the identity header is only trusted from a trusted proxy");
            }
            return match value.to_str().map(str::trim) {
                Ok(identity) if !identity.is_empty() => Ok(Actor(identity.to_owned())),
                _ => Err("the identity header is empty or not text"),
            };
        }
        if let Some(presented) = headers.get(AUTHORIZATION) {
            let Some(expected) = &self.token_sha256 else {
                return Err("the operator token is not enabled");
            };
            let token = presented
                .to_str()
                .ok()
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or_default();
            let presented: [u8; 32] = Sha256::digest(token.trim().as_bytes()).into();
            return if bool::from(presented.ct_eq(expected)) {
                Ok(Actor(TOKEN_ACTOR.into()))
            } else {
                Err("wrong operator token")
            };
        }
        match &self.dev_identity {
            Some(identity) => Ok(Actor(identity.clone())),
            None => Err("authentication required"),
        }
    }
}

fn read_token(path: &Path) -> Result<[u8; 32], StartError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| StartError::Auth(format!("operator token file {}: {e}", path.display())))?;
    let token = text.trim();
    if token.is_empty() {
        return Err(StartError::Auth(format!(
            "operator token file {} is empty",
            path.display()
        )));
    }
    Ok(Sha256::digest(token.as_bytes()).into())
}

/// Rejects every request that does not authenticate as the Operator, and records the actor of those that do.
pub async fn require_operator(
    State(auth): State<std::sync::Arc<Auth>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    match auth.authenticate(peer.ip(), request.headers()) {
        Ok(actor) => {
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        Err(reason) => {
            let mut response = error_response(StatusCode::UNAUTHORIZED, reason.into(), None);
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, "Bearer".parse().unwrap());
            response.into_response()
        }
    }
}
