//! Import of XML account exports: `<Export>` with one `<Account>` per account.
//!
//! Each account carries `Name`, `Endpoint` (`host:port`, empty for Amazon S3), `SSL`,
//! `AccessKey`, `SecretKey`, `ListAllMyBuckets`, `LastBucket` and a base64 `Tag` whose text
//! holds `Default Region=<region>`. Keys are imported when they are stored in plain text;
//! encrypted keys (a non-zero `CryptAlgo` with a key that is not plain text) cannot be read, so
//! those accounts are skipped with the reason.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use crate::account::{AccountDraft, AccountType};
use crate::bucket;
use crate::export::{ExportError, ImportPreview};

/// Most accounts one file may add.
const MAX_ACCOUNTS: usize = 1000;
/// Region used when the file does not name one.
const DEFAULT_REGION: &str = "us-east-1";
/// Endpoint of Amazon S3 accounts, which the file leaves empty.
const AMAZON_ENDPOINT: &str = "s3.amazonaws.com";

fn err(message: impl Into<String>) -> ExportError {
    ExportError(message.into())
}

/// Whether `text` is an XML document rather than a JSON export.
pub fn looks_like_xml(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('<')
}

/// Reads the accounts of an XML account export.
pub fn read(text: &str) -> Result<ImportPreview, ExportError> {
    let doc = roxmltree::Document::parse(text.trim_start_matches('\u{feff}'))
        .map_err(|e| err(format!("Import file is not valid XML: {e}")))?;
    let root = doc.root_element();
    if !root.has_tag_name("Export") {
        return Err(err(
            "Not an XML account export (the file must start with <Export>).",
        ));
    }
    let nodes: Vec<_> = root
        .children()
        .filter(|n| n.has_tag_name("Account"))
        .collect();
    if nodes.is_empty() || nodes.len() > MAX_ACCOUNTS {
        return Err(err(format!(
            "Export must contain between 1 and {MAX_ACCOUNTS} accounts."
        )));
    }
    let mut accounts = Vec::new();
    let mut skipped = Vec::new();
    for node in nodes {
        let field = |name: &str| {
            node.children()
                .find(|n| n.has_tag_name(name))
                .and_then(|n| n.text())
                .map(str::trim)
                .unwrap_or_default()
                .to_owned()
        };
        match account(&field) {
            Ok(draft) => accounts.push(draft),
            Err(reason) => {
                let name = field("Name");
                let name = if name.is_empty() {
                    field("Endpoint")
                } else {
                    name
                };
                skipped.push(format!("{name}: {reason}"));
            }
        }
    }
    Ok(ImportPreview {
        accounts,
        has_encrypted_secrets: false,
        secrets_unsupported: false,
        skipped,
    })
}

/// One account, or why it cannot be imported.
fn account(field: &dyn Fn(&str) -> String) -> Result<AccountDraft, String> {
    let access_key = field("AccessKey");
    let secret_key = field("SecretKey");
    if access_key.is_empty() {
        return Err("the file has no access key.".into());
    }
    if !keys_are_plain(&field("CryptAlgo"), &access_key) {
        return Err("its keys are encrypted; export it again without encrypting the keys.".into());
    }
    let endpoint = field("Endpoint");
    let use_tls = !field("SSL").eq_ignore_ascii_case("false");
    let amazon = endpoint.is_empty() || endpoint.to_ascii_lowercase().contains("amazonaws.com");
    let endpoint_host = if endpoint.is_empty() {
        AMAZON_ENDPOINT.to_owned()
    } else {
        without_default_port(&endpoint, use_tls).to_owned()
    };
    let name = match field("Name") {
        n if n.is_empty() => endpoint_host.clone(),
        n => n,
    };
    let mut manual_buckets = Vec::new();
    let last_bucket = bucket::normalize(&field("LastBucket"));
    // A key that may not list buckets only reaches the ones registered by hand.
    if field("ListAllMyBuckets").eq_ignore_ascii_case("false") && bucket::is_valid(&last_bucket) {
        manual_buckets.push(last_bucket);
    }
    Ok(AccountDraft {
        name,
        kind: if amazon {
            AccountType::AmazonS3
        } else {
            AccountType::S3Compatible
        },
        endpoint_host,
        use_tls,
        region: region_from_tag(&field("Tag")).unwrap_or_else(|| DEFAULT_REGION.to_owned()),
        access_key_id: access_key,
        secret_access_key: Some(secret_key).filter(|s| !s.is_empty()),
        force_path_style: !amazon,
        manual_buckets,
        ..AccountDraft::default()
    })
}

/// Plain keys come with no `CryptAlgo` (or `0`); otherwise only a key that cannot be an
/// encrypted base64 blob (no `+`, `/` or `=`) counts as plain.
fn keys_are_plain(crypt_algo: &str, access_key: &str) -> bool {
    if crypt_algo.is_empty() || crypt_algo == "0" {
        return true;
    }
    access_key.len() <= 128
        && access_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `host:443` with TLS and `host:80` without are stored as plain `host`.
fn without_default_port(endpoint: &str, use_tls: bool) -> &str {
    let default = if use_tls { ":443" } else { ":80" };
    endpoint.strip_suffix(default).unwrap_or(endpoint)
}

/// `Default Region=<region>` inside the base64 `Tag`.
fn region_from_tag(tag: &str) -> Option<String> {
    let bytes = B64.decode(tag.trim()).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let start = text.find("Default Region=")? + "Default Region=".len();
    let region: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!region.is_empty()).then_some(region)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;

    use super::{looks_like_xml, read};
    use crate::account::AccountType;

    fn tag(region: &str) -> String {
        let inner = format!(
            "<a><b></b><c>4</c><d>&lt;e&gt;&lt;f&gt;False&lt;/f&gt;&lt;g&gt;Default Region={region}&lt;/g&gt;&lt;/e&gt;</d></a>"
        );
        B64.encode(inner)
    }

    fn file(accounts: &str) -> String {
        format!(
            "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Export>\n{accounts}</Export>\n"
        )
    }

    #[test]
    fn reads_plain_accounts() {
        let text = file(&format!(
            "<Account><ID>1a2b3c4d</ID><Type>1</Type><Name>office &amp; archive</Name><SSL>True</SSL>
             <ListAllMyBuckets>False</ListAllMyBuckets>
             <AccessKey>AKIAIOSFODNN7EXAMPLE</AccessKey>
             <SecretKey>wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY</SecretKey>
             <LastBucket>po-archive</LastBucket><CryptAlgo>0</CryptAlgo>
             <Endpoint>store.example.net:443</Endpoint><Tag>{}</Tag></Account>
             <Account><Name>aws</Name><SSL>True</SSL><ListAllMyBuckets>True</ListAllMyBuckets>
             <AccessKey>AKIAI44QH8DHBEXAMPLE</AccessKey>
             <SecretKey>je7MtGbClwBF/2Zp9Utk/h3yCo8nvbEXAMPLEKEY</SecretKey>
             <LastBucket>logs</LastBucket><Endpoint></Endpoint><Tag>{}</Tag></Account>
             <Account><Name>lab</Name><SSL>False</SSL><AccessKey>minio-user</AccessKey>
             <SecretKey>minio-secret</SecretKey><Endpoint>10.0.0.5:9000</Endpoint></Account>",
            tag("ap-southeast-3"),
            tag("eu-west-1")
        ));
        assert!(looks_like_xml(&text));
        let p = read(&text).expect("read");
        assert!(p.skipped.is_empty(), "{:?}", p.skipped);
        assert_eq!(p.accounts.len(), 3);

        let a = &p.accounts[0];
        assert_eq!(a.name, "office & archive");
        assert_eq!(a.kind, AccountType::S3Compatible);
        assert_eq!(a.endpoint_host, "store.example.net");
        assert!(a.use_tls && a.force_path_style);
        assert_eq!(a.region, "ap-southeast-3");
        assert_eq!(a.access_key_id, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            a.secret_access_key.as_deref(),
            Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
        );
        assert_eq!(a.manual_buckets, ["po-archive"]);
        crate::account::validate(a).expect("valid");

        let aws = &p.accounts[1];
        assert_eq!(aws.kind, AccountType::AmazonS3);
        assert_eq!(aws.endpoint_host, "s3.amazonaws.com");
        assert!(!aws.force_path_style);
        assert_eq!(aws.region, "eu-west-1");
        assert!(aws.manual_buckets.is_empty(), "buckets can be listed");

        let lab = &p.accounts[2];
        assert!(!lab.use_tls);
        assert_eq!(lab.endpoint_host, "10.0.0.5:9000");
        assert_eq!(lab.region, "us-east-1");
        crate::account::validate(lab).expect("valid");
    }

    #[test]
    fn skips_accounts_with_encrypted_keys() {
        let text = file(
            "<Account><Name>locked</Name><SSL>True</SSL><CryptAlgo>4</CryptAlgo>
             <AccessKey>q1w2e3r4t5y6u7i8o9p0a1s2d3f4g5h6j7k8l9z0x1c2v3b4n5m6Q1W2E3R4T5Y6U7I8O9+P0A1S2D3F/G4H5=</AccessKey>
             <SecretKey>c2VjcmV0</SecretKey><Endpoint>store.example.net:443</Endpoint></Account>
             <Account><Name>plain</Name><SSL>True</SSL><CryptAlgo>4</CryptAlgo>
             <AccessKey>PLAINKEY123</AccessKey><SecretKey>abc/def+ghi=</SecretKey>
             <Endpoint>store.example.net</Endpoint></Account>",
        );
        let p = read(&text).expect("read");
        assert_eq!(p.accounts.len(), 1);
        assert_eq!(p.accounts[0].name, "plain");
        assert_eq!(
            p.accounts[0].secret_access_key.as_deref(),
            Some("abc/def+ghi=")
        );
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].starts_with("locked: "), "{}", p.skipped[0]);
        assert!(p.skipped[0].contains("encrypted"));
    }

    #[test]
    fn imported_accounts_keep_their_secret_in_the_secret_store() {
        use std::sync::Arc;

        use crate::account::{AccountService, MemoryRepository, MemorySecrets};

        let text = file(
            "<Account><Name>office</Name><SSL>True</SSL><AccessKey>OFFICEKEY1</AccessKey>
             <SecretKey>office/secret+1</SecretKey><Endpoint>store.example.net:443</Endpoint></Account>",
        );
        let service = AccountService::new(
            Box::new(MemoryRepository::default()),
            Arc::new(MemorySecrets::default()),
        );
        let saved = service
            .save(&read(&text).expect("read").accounts[0])
            .expect("save");
        assert_eq!(saved.endpoint_host, "store.example.net");
        assert_eq!(
            service.secret(saved.id).expect("secret").as_deref(),
            Some("office/secret+1")
        );
    }

    #[test]
    fn rejects_other_documents() {
        assert!(read("<Accounts/>").unwrap_err().0.contains("<Export>"));
        assert!(
            read("<Export></Export>")
                .unwrap_err()
                .0
                .contains("between 1")
        );
        assert!(
            read("<Export><Account>")
                .unwrap_err()
                .0
                .contains("not valid XML")
        );
        assert!(!looks_like_xml("{\"format\":\"s3-manager-accounts\"}"));
    }
}
