//! Auto-update for Prometheus S3Manager.
//!
//! Releases are published on GitHub with a per-user MSI and a `SHA256SUMS.txt`. The updater asks
//! the GitHub API for the latest release, downloads the MSI through the app's proxy settings,
//! checks its SHA-256 and, when this build is signed, that the MSI carries the same Authenticode
//! publisher. Installing waits for the app to exit, runs `msiexec /passive` (no UAC: the package
//! is per-user) and starts the new version.

use std::path::{Path, PathBuf};
use std::time::Duration;

use s3m_core::update::{self, Release};
use s3m_s3::ProxySettings;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// Folder name under `%LOCALAPPDATA%\Programs` the MSI installs to.
pub const INSTALL_FOLDER: &str = "Prometheus S3Manager";
/// Executable name inside the install folder.
pub const EXE_NAME: &str = "s3manager.exe";

const API: &str = "https://api.github.com";

/// Talks to GitHub for one repository.
pub struct Updater {
    client: reqwest::Client,
    repo: String,
    api: String,
}

impl Updater {
    /// Updater for `owner/name`, connecting like the S3 clients do (same proxy settings, Windows
    /// certificate store).
    pub fn new(repo: &str, proxy: &ProxySettings) -> Result<Self, String> {
        Self::with_api(repo, proxy, API)
    }

    /// Updater against another API base URL (GitHub Enterprise: `https://host/api/v3`).
    pub fn with_api(repo: &str, proxy: &ProxySettings, api: &str) -> Result<Self, String> {
        if !update::valid_repo(repo) {
            return Err(format!(
                "\"{repo}\" is not a GitHub repository (owner/name)."
            ));
        }
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("Prometheus-S3Manager/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20));
        builder = match proxy {
            ProxySettings::None => builder.no_proxy(),
            // Without a Windows proxy, reqwest falls back to HTTPS_PROXY / NO_PROXY.
            ProxySettings::System => match s3m_s3::system_proxy() {
                Some((url, no_proxy)) => builder.proxy(
                    reqwest::Proxy::all(&url)
                        .map_err(|e| format!("Invalid Windows proxy \"{url}\": {e}"))?
                        .no_proxy(reqwest::NoProxy::from_string(&no_proxy)),
                ),
                None => builder,
            },
            ProxySettings::Manual {
                url,
                user,
                password,
                no_proxy,
            } => {
                let mut p = reqwest::Proxy::all(url.trim())
                    .map_err(|e| format!("Invalid proxy URL: {e}"))?
                    .no_proxy(reqwest::NoProxy::from_string(no_proxy));
                if !user.trim().is_empty() {
                    p = p.basic_auth(user.trim(), password);
                }
                builder.proxy(p)
            }
        };
        Ok(Self {
            client: builder
                .build()
                .map_err(|e| format!("Could not set up HTTPS: {e}"))?,
            repo: repo.to_owned(),
            api: api.trim_end_matches('/').to_owned(),
        })
    }

    /// The latest final release.
    pub async fn latest(&self) -> Result<Release, String> {
        let url = format!("{}/repos/{}/releases/latest", self.api, self.repo);
        let response = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| network(&e))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(format!(
                "No published release found for {}. The repository may be private or have no releases yet.",
                self.repo
            ));
        }
        if status == reqwest::StatusCode::FORBIDDEN || status.as_u16() == 429 {
            return Err(
                "GitHub's limit for update checks is reached. Try again in an hour.".into(),
            );
        }
        if !status.is_success() {
            return Err(format!("GitHub answered HTTP {}.", status.as_u16()));
        }
        let body = response.text().await.map_err(|e| network(&e))?;
        update::parse_release(&body)
    }

    /// Downloads the release's MSI into `dir` and checks it against `SHA256SUMS.txt`.
    /// `progress` gets (bytes done, bytes total).
    pub async fn download(
        &self,
        release: &Release,
        dir: &Path,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
        cancel: &CancellationToken,
    ) -> Result<PathBuf, String> {
        let asset = release
            .installer
            .as_ref()
            .ok_or("This release has no Windows installer (.msi).")?;
        let sums = release.checksums.as_ref().ok_or(
            "This release has no SHA256SUMS.txt, so its installer cannot be verified. Download it from the release page instead.",
        )?;
        let sums = self.text(&sums.url).await?;
        let expected = update::checksum_for(&sums, &asset.name)
            .ok_or_else(|| format!("SHA256SUMS.txt has no entry for {}.", asset.name))?;
        if asset.name.contains(['/', '\\']) || asset.name.contains("..") {
            return Err(format!("Unexpected installer name \"{}\".", asset.name));
        }

        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        clean_folder(dir).await;
        let target = dir.join(&asset.name);
        let part = dir.join(format!("{}.part", asset.name));
        let result = self
            .fetch(&asset.url, &part, asset.size, progress, cancel)
            .await
            .and_then(|digest| {
                if digest == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "The downloaded installer does not match its published checksum (expected {expected}, got {digest}). Nothing was installed."
                    ))
                }
            });
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
        let _ = tokio::fs::remove_file(&target).await;
        tokio::fs::rename(&part, &target)
            .await
            .map_err(|e| format!("{}: {e}", target.display()))?;
        Ok(target)
    }

    async fn text(&self, url: &str) -> Result<String, String> {
        let response = self
            .client
            .get(url)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| network(&e))?;
        response.text().await.map_err(|e| network(&e))
    }

    /// Streams `url` into `path`; returns the hex SHA-256 of what was written.
    async fn fetch(
        &self,
        url: &str,
        path: &Path,
        size_hint: u64,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| network(&e))?;
        let total = response.content_length().unwrap_or(size_hint);
        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        let mut done = 0u64;
        progress(0, total);
        loop {
            let chunk = tokio::select! {
                () = cancel.cancelled() => return Err("Download canceled.".into()),
                chunk = tokio::time::timeout(Duration::from_secs(60), response.chunk()) => chunk,
            };
            let chunk = match chunk {
                Err(_) => return Err("The download stalled for a minute and was stopped.".into()),
                Ok(Err(e)) => return Err(network(&e)),
                Ok(Ok(None)) => break,
                Ok(Ok(Some(bytes))) => bytes,
            };
            digest.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("{}: {e}", path.display()))?;
            done += chunk.len() as u64;
            progress(done, total.max(done));
        }
        file.flush()
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(update::hex(digest.finish().as_ref()))
    }
}

/// Removes leftovers of earlier updates.
async fn clean_folder(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".msi") || name.ends_with(".part") {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

fn network(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    format!("Could not reach GitHub: {text}")
}

/// Folder the MSI installs to: `%LOCALAPPDATA%\Programs\Prometheus S3Manager`.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join(INSTALL_FOLDER))
}

/// Whether `exe` runs from the MSI install folder (only then can it update itself).
pub fn is_installed(exe: &Path) -> bool {
    let (Some(dir), Some(parent)) = (install_dir(), exe.parent()) else {
        return false;
    };
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    norm(parent) == norm(&dir)
}

/// Folder updates are downloaded to.
pub fn download_dir() -> PathBuf {
    std::env::temp_dir().join("Prometheus-S3Manager-update")
}

/// Outcome of comparing the Authenticode signatures of the running app and the installer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureCheck {
    /// The installer may be used.
    pub accepted: bool,
    /// User-facing explanation.
    pub message: String,
}

/// Status ("Valid", "NotSigned", ...) and signer subject of the app and the installer, as
/// reported by PowerShell's `Get-AuthenticodeSignature`.
pub fn judge_signatures(
    exe_status: &str,
    exe_signer: &str,
    msi_status: &str,
    msi_signer: &str,
) -> SignatureCheck {
    if exe_status == "Valid" {
        if msi_status == "Valid" && msi_signer == exe_signer {
            SignatureCheck {
                accepted: true,
                message: format!("Installer signed by {msi_signer}."),
            }
        } else {
            SignatureCheck {
                accepted: false,
                message: format!(
                    "The installer is not signed by the same publisher as this app (app: {exe_signer}; installer: {} {msi_signer}). Nothing was installed.",
                    if msi_status == "Valid" {
                        "signed by"
                    } else {
                        msi_status
                    }
                ),
            }
        }
    } else {
        SignatureCheck {
            accepted: true,
            message: if msi_status == "Valid" {
                format!("Installer signed by {msi_signer}.")
            } else {
                "This build is not code-signed; the installer was checked with its SHA-256 checksum only.".into()
            },
        }
    }
}

/// Compares the Authenticode signatures of `exe` (the running app) and `msi`.
#[cfg(windows)]
pub async fn check_signatures(exe: &Path, msi: &Path) -> Result<SignatureCheck, String> {
    #[derive(serde::Deserialize)]
    struct Report {
        exe: String,
        #[serde(rename = "exeSigner", default)]
        exe_signer: Option<String>,
        msi: String,
        #[serde(rename = "msiSigner", default)]
        msi_signer: Option<String>,
    }
    const SCRIPT: &str = "$ErrorActionPreference = 'Stop'; \
        $e = Get-AuthenticodeSignature -LiteralPath $env:S3M_EXE; \
        $m = Get-AuthenticodeSignature -LiteralPath $env:S3M_MSI; \
        [pscustomobject]@{ exe = [string]$e.Status; exeSigner = [string]$e.SignerCertificate.Subject; \
        msi = [string]$m.Status; msiSigner = [string]$m.SignerCertificate.Subject } | ConvertTo-Json -Compress";
    let output = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .env("S3M_EXE", exe)
        .env("S3M_MSI", msi)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .await
        .map_err(|e| format!("Could not check the installer's signature: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let report: Report = serde_json::from_str(text.trim()).map_err(|_| {
        format!(
            "Could not check the installer's signature: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    Ok(judge_signatures(
        &report.exe,
        report.exe_signer.as_deref().unwrap_or(""),
        &report.msi,
        report.msi_signer.as_deref().unwrap_or(""),
    ))
}

/// Compares the Authenticode signatures of `exe` (the running app) and `msi`.
#[cfg(not(windows))]
pub async fn check_signatures(_exe: &Path, _msi: &Path) -> Result<SignatureCheck, String> {
    Err("Installing updates is only supported on Windows.".into())
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts a hidden helper that waits for this process to exit, runs the MSI with a progress
/// bar only (`/passive`, logged to `log`) and starts `exe` again. The caller then closes the app.
#[cfg(windows)]
pub fn start_install(msi: &Path, exe: &Path, log: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const SCRIPT: &str = "$ErrorActionPreference = 'SilentlyContinue'; \
        Wait-Process -Id ([int]$env:S3M_PID) -Timeout 120; \
        $a = @('/i', ('\"{0}\"' -f $env:S3M_MSI), '/passive', '/norestart', '/l*v', ('\"{0}\"' -f $env:S3M_LOG)); \
        Start-Process -FilePath 'msiexec.exe' -ArgumentList $a -Wait; \
        Start-Process -FilePath $env:S3M_EXE";
    let spawn = |flags: u32| {
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-Command",
                SCRIPT,
            ])
            .env("S3M_PID", std::process::id().to_string())
            .env("S3M_MSI", msi)
            .env("S3M_EXE", exe)
            .env("S3M_LOG", log)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    // Leave the app's job object when allowed, so the helper survives the app closing.
    spawn(base | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(base))
        .map(drop)
        .map_err(|e| format!("Could not start the installer: {e}"))
}

/// Starts the installer (Windows only).
#[cfg(not(windows))]
pub fn start_install(_msi: &Path, _exe: &Path, _log: &Path) -> Result<(), String> {
    Err("Installing updates is only supported on Windows.".into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn signature_rules() {
        let ok = judge_signatures("Valid", "CN=Acme", "Valid", "CN=Acme");
        assert!(ok.accepted);
        let other = judge_signatures("Valid", "CN=Acme", "Valid", "CN=Evil");
        assert!(!other.accepted, "{}", other.message);
        let unsigned_msi = judge_signatures("Valid", "CN=Acme", "NotSigned", "");
        assert!(!unsigned_msi.accepted);
        assert!(unsigned_msi.message.contains("NotSigned"));
        let dev = judge_signatures("NotSigned", "", "NotSigned", "");
        assert!(dev.accepted);
        assert!(dev.message.contains("SHA-256"));
    }

    #[test]
    fn install_location() {
        let dir = install_dir().expect("LOCALAPPDATA");
        assert!(is_installed(&dir.join(EXE_NAME)));
        assert!(!is_installed(Path::new(
            r"D:\build\target\release\s3manager.exe"
        )));
        assert!(Updater::new("not a repo", &ProxySettings::None).is_err());
    }

    /// Minimal HTTP server: GitHub API, checksum file and installer.
    async fn serve(msi: Vec<u8>, sums: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let release = format!(
            r#"{{"tag_name":"v9.9.9","name":"Big one","body":"notes","html_url":"{base}/page","draft":false,"prerelease":false,
            "assets":[{{"name":"Prometheus-S3Manager-9.9.9-x64.msi","browser_download_url":"{base}/msi","size":{}}},
                      {{"name":"SHA256SUMS.txt","browser_download_url":"{base}/sums","size":1}}]}}"#,
            msi.len()
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (release, msi, sums) = (release.clone(), msi.clone(), sums.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let (status, body): (&str, Vec<u8>) = match path.as_str() {
                        "/repos/acme/s3m/releases/latest" => ("200 OK", release.into_bytes()),
                        "/msi" => ("200 OK", msi),
                        "/sums" => ("200 OK", sums.into_bytes()),
                        _ => ("404 Not Found", Vec::new()),
                    };
                    let header = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        base
    }

    fn sha(data: &[u8]) -> String {
        update::hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
    }

    #[tokio::test]
    async fn checks_downloads_and_verifies() {
        let msi = vec![42u8; 300_000];
        let sums = format!("{}  Prometheus-S3Manager-9.9.9-x64.msi\n", sha(&msi));
        let base = serve(msi.clone(), sums).await;
        let updater = Updater::with_api("acme/s3m", &ProxySettings::None, &base).unwrap();
        let release = updater.latest().await.unwrap();
        assert_eq!(release.name, "Big one");
        assert!(update::is_newer(&release, "0.2.0"));

        let dir = std::env::temp_dir().join(format!("s3m-upd-{}", std::process::id()));
        let seen = Arc::new(AtomicU64::new(0));
        let s = seen.clone();
        let path = updater
            .download(
                &release,
                &dir,
                &move |done, _| s.store(done, Ordering::Relaxed),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), msi);
        assert_eq!(seen.load(Ordering::Relaxed), msi.len() as u64);
        let _ = std::fs::remove_dir_all(&dir);

        let missing = Updater::with_api("acme/other", &ProxySettings::None, &base).unwrap();
        assert!(
            missing
                .latest()
                .await
                .unwrap_err()
                .contains("No published release")
        );
    }

    #[tokio::test]
    async fn rejects_a_tampered_installer() {
        let msi = vec![1u8; 1000];
        let sums = format!("{}  Prometheus-S3Manager-9.9.9-x64.msi\n", "0".repeat(64));
        let base = serve(msi, sums).await;
        let updater = Updater::with_api("acme/s3m", &ProxySettings::None, &base).unwrap();
        let release = updater.latest().await.unwrap();
        let dir = std::env::temp_dir().join(format!("s3m-upd-bad-{}", std::process::id()));
        let err = updater
            .download(&release, &dir, &|_, _| {}, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        let leftovers = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
