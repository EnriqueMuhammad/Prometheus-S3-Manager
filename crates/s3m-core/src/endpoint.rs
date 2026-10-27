//! Endpoint rules: HTTPS by default; plain HTTP only when the user unchecks SSL/TLS, and then
//! only to private addresses. Host names over HTTP are accepted here and enforced at connect
//! time by resolving DNS (the private-network guard in `s3m-s3`).

use crate::network;

/// What the endpoint box and the TLS checkbox amount to after normalisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointFields {
    /// Host with optional port, without scheme or trailing slash.
    pub host: String,
    /// Whether to use HTTPS.
    pub use_tls: bool,
}

/// Outcome of [`validate`] for a well-formed endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointCheck {
    /// Plain HTTP to a host name: every connection must resolve to private addresses only.
    pub requires_private_resolution: bool,
}

/// Normalises what was typed in the endpoint box. A typed scheme wins over the checkbox and is
/// stripped, so pasting `http://minio:9000` does what it says.
pub fn normalize(input: &str, use_tls: bool) -> EndpointFields {
    let mut text = input.trim();
    let mut use_tls = use_tls;
    if let Some(pos) = text.find("://") {
        let scheme = &text[..pos];
        let is_scheme = scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
        if is_scheme {
            match scheme.to_ascii_lowercase().as_str() {
                "http" => use_tls = false,
                "https" => use_tls = true,
                _ => {}
            }
            text = &text[pos + 3..];
        }
    }
    EndpointFields {
        host: text.trim_end_matches('/').to_owned(),
        use_tls,
    }
}

/// Validates a normalised host[:port]. Returns a user-facing message on failure.
pub fn validate(host: &str, use_tls: bool) -> Result<EndpointCheck, String> {
    if host.trim().is_empty() {
        return Err("API endpoint is required.".into());
    }
    if host.contains('@') {
        return Err("Endpoint must not contain credentials.".into());
    }
    if host.contains(['/', '?', '#']) {
        return Err("Enter only the host name and optional port, without a path.".into());
    }
    let port =
        parse_host_port(host).ok_or_else(|| format!("\"{host}\" is not a valid host name."))?;
    if let Some(port) = port
        && !(1..=65535).contains(&port)
    {
        return Err("Port must be between 1 and 65535.".into());
    }
    if use_tls {
        return Ok(EndpointCheck {
            requires_private_resolution: false,
        });
    }
    let host_only = host_without_port(host);
    if let Some(ip) = network::parse_literal(host_only) {
        return if network::is_private(ip) {
            Ok(EndpointCheck {
                requires_private_resolution: false,
            })
        } else {
            Err("Plain HTTP is only allowed to private-network addresses. Enable SSL/TLS for this endpoint.".into())
        };
    }
    Ok(EndpointCheck {
        requires_private_resolution: !host_only.eq_ignore_ascii_case("localhost"),
    })
}

/// Checks the host[:port] shape. `Some(port)` is the parsed port (possibly out of range),
/// `None` when the shape is invalid.
fn parse_host_port(text: &str) -> Option<Option<u32>> {
    let (host_ok, rest) = if let Some(inner_end) = text.strip_prefix('[').and_then(|t| t.find(']'))
    {
        let inner = &text[1..=inner_end];
        let ok = !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
        (ok, &text[inner_end + 2..])
    } else {
        let (name, rest) = match text.find(':') {
            Some(i) => (&text[..i], &text[i..]),
            None => (text, ""),
        };
        let bytes = name.as_bytes();
        let ok = (1..=253).contains(&bytes.len())
            && bytes[0].is_ascii_alphanumeric()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'-');
        (ok, rest)
    };
    if !host_ok {
        return None;
    }
    if rest.is_empty() {
        return Some(None);
    }
    let digits = rest.strip_prefix(':')?;
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().map(Some)
}

/// Host part of host[:port], without IPv6 brackets.
pub fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.find(']').map_or(host, |end| &rest[..end]);
    }
    match (host.find(':'), host.rfind(':')) {
        (Some(first), Some(last)) if first == last && first > 0 => &host[..first],
        _ => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_scheme_wins() {
        assert_eq!(
            normalize(" http://minio:9000/ ", true),
            EndpointFields {
                host: "minio:9000".into(),
                use_tls: false
            }
        );
        assert!(normalize("HTTPS://store.example.net", false).use_tls);
        assert_eq!(
            normalize("store.example.net", true).host,
            "store.example.net"
        );
    }

    #[test]
    fn shape_rules() {
        assert!(validate("store.example.net", true).is_ok());
        assert!(validate("10.0.0.5:9000", true).is_ok());
        assert!(validate("[fd00::1]:9000", true).is_ok());
        assert_eq!(validate("", true).unwrap_err(), "API endpoint is required.");
        assert!(
            validate("user@host", true)
                .unwrap_err()
                .contains("credentials")
        );
        assert!(
            validate("host/path", true)
                .unwrap_err()
                .contains("without a path")
        );
        assert!(
            validate("-bad", true)
                .unwrap_err()
                .contains("not a valid host")
        );
        assert!(validate("host:70000", true).unwrap_err().contains("Port"));
        assert!(validate("host:0", true).unwrap_err().contains("Port"));
        assert!(
            validate("host:123456", true)
                .unwrap_err()
                .contains("not a valid host")
        );
    }

    #[test]
    fn http_only_to_private() {
        assert_eq!(
            validate("10.0.0.5:9000", false),
            Ok(EndpointCheck {
                requires_private_resolution: false
            })
        );
        assert!(
            validate("8.8.8.8", false)
                .unwrap_err()
                .contains("private-network")
        );
        assert_eq!(
            validate("minio.internal:9000", false),
            Ok(EndpointCheck {
                requires_private_resolution: true
            })
        );
        assert_eq!(
            validate("localhost:9000", false),
            Ok(EndpointCheck {
                requires_private_resolution: false
            })
        );
    }

    #[test]
    fn strips_port() {
        assert_eq!(host_without_port("a.b:9000"), "a.b");
        assert_eq!(host_without_port("[::1]:9000"), "::1");
        assert_eq!(host_without_port("a.b"), "a.b");
    }
}
