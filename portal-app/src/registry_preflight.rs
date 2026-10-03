//! Read-only OCI registry checks for the configured worker image.
//!
//! Public registries commonly challenge anonymous clients with HTTP 401 and a
//! `WWW-Authenticate: Bearer ...` header. That does not mean an image is
//! private: an anonymous pull token may still grant access. This module follows
//! exactly that standard exchange and requests only the manifest for the exact
//! configured tag. It never downloads image layers and never needs a GitHub
//! credential.

use std::time::Duration;

const GHCR_TOKEN_REALM: &str = "https://ghcr.io/token";
const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.docker.distribution.manifest.v2+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicImageStatus {
    Available,
    Unauthorized,
    TagMissing,
    RegistryUnavailable,
    InvalidResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicImageCheck {
    pub status: PublicImageStatus,
    pub detail: String,
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageReference {
    repository: String,
    tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BearerChallenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(rename = "access_token", default)]
    access_token: Option<String>,
}

/// Check whether an exact `ghcr.io/owner/name:tag` reference can be pulled
/// anonymously. The returned value is domain-level data so callers never need
/// to infer setup state from an HTTP error string.
pub fn check_public_ghcr_image(image: &str) -> PublicImageCheck {
    check_public_image_at(image, "https://ghcr.io", GHCR_TOKEN_REALM)
}

fn check_public_image_at(
    image: &str,
    registry_base_url: &str,
    allowed_token_realm: &str,
) -> PublicImageCheck {
    let reference = match parse_ghcr_reference(image) {
        Ok(reference) => reference,
        Err(detail) => {
            return PublicImageCheck {
                status: PublicImageStatus::InvalidResponse,
                detail,
                http_status: None,
            };
        }
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // Keeping non-success responses lets us inspect GHCR's authentication
        // challenge instead of losing its headers in a generic status error.
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(45)))
        .build()
        .into();
    let manifest_url = format!(
        "{registry_base_url}/v2/{}/manifests/{}",
        reference.repository, reference.tag
    );

    let first = match request_manifest(&agent, &manifest_url, None) {
        Ok(response) => response,
        Err(error) => return unavailable(error),
    };
    if first.status().as_u16() != 401 {
        return classify_manifest_response(&first);
    }

    let challenge_header = match first
        .headers()
        .get("www-authenticate")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value,
        None => {
            return PublicImageCheck {
                status: PublicImageStatus::Unauthorized,
                detail: String::from(
                    "GHCR denied anonymous manifest access without a Bearer challenge.",
                ),
                http_status: Some(401),
            };
        }
    };
    let challenge = match parse_bearer_challenge(challenge_header) {
        Some(challenge) => challenge,
        None => {
            return PublicImageCheck {
                status: PublicImageStatus::InvalidResponse,
                detail: String::from("GHCR returned an invalid Bearer authentication challenge."),
                http_status: Some(401),
            };
        }
    };

    // A registry controls this header, but following an arbitrary token realm
    // would turn a harmless preflight into an unexpected outbound request.
    // GHCR's documented realm is therefore explicitly allow-listed.
    if challenge.realm != allowed_token_realm {
        return PublicImageCheck {
            status: PublicImageStatus::InvalidResponse,
            detail: String::from("GHCR returned an unexpected token service URL."),
            http_status: Some(401),
        };
    }
    let required_scope = format!("repository:{}:pull", reference.repository);
    if challenge
        .scope
        .as_deref()
        .is_some_and(|scope| scope != required_scope)
    {
        return PublicImageCheck {
            status: PublicImageStatus::InvalidResponse,
            detail: String::from("GHCR requested an unexpected repository scope."),
            http_status: Some(401),
        };
    }
    let service = challenge.service.as_deref().unwrap_or("ghcr.io");
    if service != "ghcr.io" {
        return PublicImageCheck {
            status: PublicImageStatus::InvalidResponse,
            detail: String::from("GHCR returned an unexpected token service name."),
            http_status: Some(401),
        };
    }

    let token_response = match agent
        .get(&challenge.realm)
        .query("service", service)
        .query("scope", &required_scope)
        .call()
    {
        Ok(response) => response,
        Err(error) => return unavailable(error),
    };
    if token_response.status().as_u16() != 200 {
        return PublicImageCheck {
            status: PublicImageStatus::Unauthorized,
            detail: String::from("GHCR did not issue an anonymous pull token for this package."),
            http_status: Some(token_response.status().as_u16()),
        };
    }
    let mut token_response = token_response;
    let token = match token_response.body_mut().read_json::<TokenResponse>() {
        Ok(value) => value.token.or(value.access_token),
        Err(_) => None,
    };
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return PublicImageCheck {
            status: PublicImageStatus::InvalidResponse,
            detail: String::from("GHCR returned an invalid anonymous token response."),
            http_status: Some(200),
        };
    };

    match request_manifest(&agent, &manifest_url, Some(&token)) {
        Ok(response) => classify_manifest_response(&response),
        Err(error) => unavailable(error),
    }
}

fn request_manifest(
    agent: &ureq::Agent,
    url: &str,
    token: Option<&str>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let request = agent.get(url).header("Accept", MANIFEST_ACCEPT);
    match token {
        Some(token) => request
            .header("Authorization", &format!("Bearer {token}"))
            .call(),
        None => request.call(),
    }
}

fn classify_manifest_response(response: &ureq::http::Response<ureq::Body>) -> PublicImageCheck {
    let status = response.status().as_u16();
    match status {
        200 => {
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(str::trim);
            if content_type.is_some_and(is_manifest_media_type) {
                PublicImageCheck {
                    status: PublicImageStatus::Available,
                    detail: String::from("The exact worker tag is anonymously readable from GHCR."),
                    http_status: Some(status),
                }
            } else {
                PublicImageCheck {
                    status: PublicImageStatus::InvalidResponse,
                    detail: String::from("GHCR returned an unsupported manifest media type."),
                    http_status: Some(status),
                }
            }
        }
        401 | 403 => PublicImageCheck {
            status: PublicImageStatus::Unauthorized,
            detail: String::from(
                "The GHCR package exists but is not anonymously readable; verify that it is public.",
            ),
            http_status: Some(status),
        },
        404 => PublicImageCheck {
            status: PublicImageStatus::TagMissing,
            detail: String::from("The configured worker image tag does not exist on GHCR."),
            http_status: Some(status),
        },
        500..=599 => PublicImageCheck {
            status: PublicImageStatus::RegistryUnavailable,
            detail: String::from("GHCR is temporarily unavailable."),
            http_status: Some(status),
        },
        _ => PublicImageCheck {
            status: PublicImageStatus::InvalidResponse,
            detail: format!("GHCR returned unexpected HTTP status {status}."),
            http_status: Some(status),
        },
    }
}

fn unavailable(error: ureq::Error) -> PublicImageCheck {
    PublicImageCheck {
        status: PublicImageStatus::RegistryUnavailable,
        detail: format!("Could not reach GHCR: {error}"),
        http_status: None,
    }
}

fn parse_ghcr_reference(image: &str) -> Result<ImageReference, String> {
    let rest = image
        .strip_prefix("ghcr.io/")
        .ok_or_else(|| String::from("Worker image must use the ghcr.io registry."))?;
    let (repository, tag) = rest.rsplit_once(':').ok_or_else(|| {
        String::from("Worker image must include an explicit immutable version tag.")
    })?;
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '/')
            })
    };
    if !valid_part(repository) || !valid_part(tag) || !repository.contains('/') {
        return Err(String::from("Worker image reference is invalid."));
    }
    Ok(ImageReference {
        repository: repository.to_string(),
        tag: tag.to_string(),
    })
}

fn is_manifest_media_type(value: &str) -> bool {
    matches!(
        value,
        "application/vnd.oci.image.manifest.v1+json"
            | "application/vnd.oci.image.index.v1+json"
            | "application/vnd.docker.distribution.manifest.v2+json"
            | "application/vnd.docker.distribution.manifest.list.v2+json"
    )
}

fn parse_bearer_challenge(value: &str) -> Option<BearerChallenge> {
    let value = value.trim();
    let fields = value
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case("bearer"))
        .and_then(|_| value.get(6..))?
        .trim();
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for field in split_challenge_fields(fields)? {
        let (key, raw_value) = field.split_once('=')?;
        let decoded = raw_value
            .trim()
            .strip_prefix('"')?
            .strip_suffix('"')?
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
        match key.trim().to_ascii_lowercase().as_str() {
            "realm" => realm = Some(decoded),
            "service" => service = Some(decoded),
            "scope" => scope = Some(decoded),
            _ => {}
        }
    }
    Some(BearerChallenge {
        realm: realm?,
        service,
        scope,
    })
}

fn split_challenge_fields(value: &str) -> Option<Vec<&str>> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(value[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted || escaped {
        return None;
    }
    fields.push(value[start..].trim());
    Some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn response(status: u16, content_type: Option<&str>) -> ureq::http::Response<ureq::Body> {
        let mut builder = ureq::http::Response::builder().status(status);
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        builder.body(ureq::Body::builder().data(&[][..])).unwrap()
    }

    #[test]
    fn exact_public_worker_reference_is_parsed() {
        let parsed = parse_ghcr_reference("ghcr.io/jakob-plappert/portal-comfy-worker:0.6.0")
            .expect("valid GHCR reference should parse");
        assert_eq!(parsed.repository, "jakob-plappert/portal-comfy-worker");
        assert_eq!(parsed.tag, "0.6.0");
    }

    #[test]
    fn bearer_challenge_preserves_anonymous_pull_scope() {
        let parsed = parse_bearer_challenge(
            "Bearer realm=\"https://ghcr.io/token\",service=\"ghcr.io\",scope=\"repository:jakob-plappert/portal-comfy-worker:pull\"",
        )
        .expect("standard challenge should parse");
        assert_eq!(parsed.realm, GHCR_TOKEN_REALM);
        assert_eq!(parsed.service.as_deref(), Some("ghcr.io"));
        assert_eq!(
            parsed.scope.as_deref(),
            Some("repository:jakob-plappert/portal-comfy-worker:pull")
        );
    }

    #[test]
    fn current_oci_and_docker_manifest_types_are_accepted() {
        for media_type in [
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.oci.image.index.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
            "application/vnd.docker.distribution.manifest.list.v2+json",
        ] {
            assert_eq!(
                classify_manifest_response(&response(200, Some(media_type))).status,
                PublicImageStatus::Available
            );
        }
    }

    #[test]
    fn missing_worker_tag_is_distinct_from_auth_failure() {
        assert_eq!(
            classify_manifest_response(&response(404, None)).status,
            PublicImageStatus::TagMissing
        );
        assert_eq!(
            classify_manifest_response(&response(401, None)).status,
            PublicImageStatus::Unauthorized
        );
    }

    #[test]
    fn anonymous_bearer_challenge_is_followed_and_manifest_is_retried() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture server should bind");
        let address = listener
            .local_addr()
            .expect("fixture address should be available");
        let base_url = format!("http://{address}");
        let token_realm = format!("{base_url}/token");
        let server_realm = token_realm.clone();
        let server = thread::spawn(move || {
            for step in 0..3 {
                let (mut stream, _) = listener.accept().expect("fixture request should arrive");
                let mut request = [0_u8; 4096];
                let count = stream
                    .read(&mut request)
                    .expect("request should be readable");
                let request = String::from_utf8_lossy(&request[..count]);
                let response = match step {
                    0 => {
                        assert!(request.starts_with(
                            "GET /v2/jakob-plappert/portal-comfy-worker/manifests/0.6.0"
                        ));
                        format!(
                            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"{server_realm}\",service=\"ghcr.io\",scope=\"repository:jakob-plappert/portal-comfy-worker:pull\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                    }
                    1 => {
                        assert!(request.starts_with("GET /token?"));
                        assert!(request.contains("service=ghcr.io"));
                        assert!(request.contains(
                            "scope=repository%3Ajakob-plappert%2Fportal-comfy-worker%3Apull"
                        ));
                        let body = r#"{"token":"anonymous-fixture-token"}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    _ => {
                        assert!(
                            request.contains("authorization: Bearer anonymous-fixture-token")
                                || request
                                    .contains("Authorization: Bearer anonymous-fixture-token")
                        );
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_string()
                    }
                };
                stream
                    .write_all(response.as_bytes())
                    .expect("fixture response should be writable");
            }
        });

        let check = check_public_image_at(
            "ghcr.io/jakob-plappert/portal-comfy-worker:0.6.0",
            &base_url,
            &token_realm,
        );
        server.join().expect("fixture server should finish");
        assert_eq!(check.status, PublicImageStatus::Available);
        assert_eq!(check.http_status, Some(200));
    }
}
