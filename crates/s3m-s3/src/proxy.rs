//! HTTP proxy for TLS accounts. Plain-HTTP accounts (private network only) never use a proxy.

use aws_smithy_http_client::proxy::ProxyConfig;

use crate::error::{StorageError, StorageErrorKind};

/// How TLS connections reach the internet.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub enum ProxySettings {
    /// Direct connections.
    #[default]
    None,
    /// Windows proxy settings (Internet Options), else `HTTPS_PROXY` / `NO_PROXY`.
    System,
    /// An explicit proxy.
    Manual {
        /// `http://host:port` (or `https://`).
        url: String,
        /// User for basic authentication (empty = none).
        user: String,
        /// Password for basic authentication.
        password: String,
        /// Hosts that bypass the proxy, comma separated (`*.corp.local, 10.0.0.0/8`).
        no_proxy: String,
    },
}

/// Checks a manual proxy URL. The message is user-facing.
pub fn validate_proxy_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(
            "The proxy URL must start with http:// or https://, e.g. http://proxy.corp.local:8080"
                .into(),
        );
    }
    ProxyConfig::all(url)
        .map(|_| ())
        .map_err(|e| format!("Invalid proxy URL: {e}"))
}

/// Proxy server and bypass list from the Windows Internet Options.
#[cfg(windows)]
fn windows_proxy() -> Option<(String, String)> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    let overrides: String = key.get_value("ProxyOverride").unwrap_or_default();
    Some(parse_windows_proxy(&server, &overrides))
}

#[cfg(not(windows))]
fn windows_proxy() -> Option<(String, String)> {
    None
}

/// Proxy URL and bypass list from the Windows Internet Options, when a proxy is enabled there.
pub fn system_proxy() -> Option<(String, String)> {
    windows_proxy()
}

/// `ProxyServer` is `host:port` or `http=host:port;https=host:port`; `ProxyOverride` is a
/// `;`-separated list where `<local>` means plain host names.
fn parse_windows_proxy(server: &str, overrides: &str) -> (String, String) {
    let pick = server
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("https="))
        .or_else(|| {
            server
                .split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("http="))
        })
        .unwrap_or_else(|| server.split(';').next().unwrap_or("").trim());
    let url = if pick.contains("://") {
        pick.to_owned()
    } else {
        format!("http://{pick}")
    };
    let no_proxy = overrides
        .split(';')
        .map(str::trim)
        .filter(|o| !o.is_empty() && *o != "<local>")
        .collect::<Vec<_>>()
        .join(",");
    (url, no_proxy)
}

/// The smithy proxy configuration for the settings.
pub(crate) fn proxy_config(settings: &ProxySettings) -> Result<ProxyConfig, StorageError> {
    let invalid = |e: String| {
        StorageError::with_hint(
            StorageErrorKind::InvalidRequest,
            e,
            "Check the proxy in Tools > Options.",
        )
    };
    match settings {
        ProxySettings::None => Ok(ProxyConfig::disabled()),
        ProxySettings::System => match windows_proxy() {
            Some((url, no_proxy)) => {
                let mut config = ProxyConfig::all(url.as_str())
                    .map_err(|e| invalid(format!("Invalid Windows proxy \"{url}\": {e}")))?;
                if !no_proxy.is_empty() {
                    config = config.no_proxy(no_proxy);
                }
                Ok(config)
            }
            None => Ok(ProxyConfig::from_env()),
        },
        ProxySettings::Manual {
            url,
            user,
            password,
            no_proxy,
        } => {
            validate_proxy_url(url).map_err(invalid)?;
            let mut config = ProxyConfig::all(url.trim()).map_err(|e| invalid(e.to_string()))?;
            if !user.trim().is_empty() {
                config = config.with_basic_auth(user.trim(), password.as_str());
            }
            if !no_proxy.trim().is_empty() {
                config = config.no_proxy(no_proxy.trim());
            }
            Ok(config)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_proxy_formats() {
        assert_eq!(
            parse_windows_proxy("proxy.corp:8080", "<local>;*.corp.local;10.*"),
            ("http://proxy.corp:8080".into(), "*.corp.local,10.*".into())
        );
        assert_eq!(
            parse_windows_proxy("http=p1:80;https=p2:443", ""),
            ("http://p2:443".into(), String::new())
        );
    }

    #[test]
    fn url_validation() {
        assert!(validate_proxy_url("http://proxy:8080").is_ok());
        assert!(validate_proxy_url("proxy:8080").is_err());
        assert!(
            proxy_config(&ProxySettings::None)
                .expect("none")
                .is_disabled()
        );
    }
}
