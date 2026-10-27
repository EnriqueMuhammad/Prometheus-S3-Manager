//! Endpoint presets for common S3-compatible providers (account editor). Placeholders in
//! capitals (`ACCOUNT_ID`, `REGION`) are replaced by the user.

use crate::account::AccountType;

/// A provider preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// Name in the combo box.
    pub name: &'static str,
    /// Account type.
    pub kind: AccountType,
    /// Endpoint, possibly with a placeholder.
    pub endpoint: &'static str,
    /// Default region.
    pub region: &'static str,
    /// Path-style requests.
    pub path_style: bool,
    /// SSL/TLS.
    pub tls: bool,
    /// What to fill in.
    pub hint: &'static str,
}

/// Known presets.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Amazon S3",
        kind: AccountType::AmazonS3,
        endpoint: "s3.amazonaws.com",
        region: "us-east-1",
        path_style: false,
        tls: true,
        hint: "Set the region of your buckets, e.g. ap-southeast-3 (Jakarta).",
    },
    Preset {
        name: "Cloudflare R2",
        kind: AccountType::S3Compatible,
        endpoint: "ACCOUNT_ID.r2.cloudflarestorage.com",
        region: "auto",
        path_style: true,
        tls: true,
        hint: "Replace ACCOUNT_ID with your Cloudflare account id (R2 > Overview).",
    },
    Preset {
        name: "Wasabi",
        kind: AccountType::S3Compatible,
        endpoint: "s3.REGION.wasabisys.com",
        region: "ap-southeast-1",
        path_style: false,
        tls: true,
        hint: "Replace REGION with the bucket region, e.g. ap-southeast-1, and set the same region below.",
    },
    Preset {
        name: "DigitalOcean Spaces",
        kind: AccountType::S3Compatible,
        endpoint: "REGION.digitaloceanspaces.com",
        region: "sgp1",
        path_style: false,
        tls: true,
        hint: "Replace REGION with the Spaces region, e.g. sgp1, and set the same region below.",
    },
    Preset {
        name: "Backblaze B2",
        kind: AccountType::S3Compatible,
        endpoint: "s3.REGION.backblazeb2.com",
        region: "us-west-004",
        path_style: false,
        tls: true,
        hint: "Use the S3 endpoint shown on the bucket page; the region is the part after \"s3.\".",
    },
    Preset {
        name: "Google Cloud Storage (HMAC)",
        kind: AccountType::S3Compatible,
        endpoint: "storage.googleapis.com",
        region: "auto",
        path_style: true,
        tls: true,
        hint: "Use an HMAC key (Cloud Storage > Settings > Interoperability).",
    },
    Preset {
        name: "MinIO / local server",
        kind: AccountType::S3Compatible,
        endpoint: "HOST:9000",
        region: "us-east-1",
        path_style: true,
        tls: true,
        hint: "Replace HOST with the server address. Uncheck SSL/TLS only for a server on the private network.",
    },
];

/// Whether an endpoint still contains a placeholder.
pub fn has_placeholder(endpoint: &str) -> bool {
    ["ACCOUNT_ID", "REGION", "HOST"]
        .iter()
        .any(|p| endpoint.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_become_valid_endpoints() {
        for p in PRESETS {
            let filled = p
                .endpoint
                .replace("ACCOUNT_ID", "abc123")
                .replace("REGION", p.region)
                .replace("HOST", "10.0.0.5");
            assert!(
                crate::endpoint::validate(&filled, p.tls).is_ok(),
                "{}: {filled}",
                p.name
            );
            assert!(!has_placeholder(&filled));
        }
        assert!(has_placeholder(PRESETS[1].endpoint));
    }
}
