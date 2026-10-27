//! Bucket and object administration models: ACL, CORS, lifecycle, default encryption and
//! bucket policy, with validation. The S3 layer converts them to and from the SDK types.

use serde::{Deserialize, Serialize};

/// Grantee group URI for any authenticated AWS account.
pub const AUTHENTICATED_USERS: &str = "http://acs.amazonaws.com/groups/global/AuthenticatedUsers";
/// Grantee group URI for everyone (public).
pub const ALL_USERS: &str = "http://acs.amazonaws.com/groups/global/AllUsers";

/// Permissions in matrix column order.
pub const PERMISSIONS: [&str; 4] = ["READ", "WRITE", "READ_ACP", "WRITE_ACP"];

/// One ACL grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclGrant {
    /// `CanonicalUser`, `Group` or `AmazonCustomerByEmail`.
    pub kind: String,
    /// Canonical id, group URI or e-mail address.
    pub grantee: String,
    /// Display name when the server reports one.
    pub display_name: Option<String>,
    /// `READ`, `WRITE`, `READ_ACP`, `WRITE_ACP` or `FULL_CONTROL`.
    pub permission: String,
}

/// Access control list of a bucket or object.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Acl {
    /// Owner canonical id.
    pub owner_id: String,
    /// Owner display name.
    pub owner_name: Option<String>,
    /// Grants.
    pub grants: Vec<AclGrant>,
}

/// The ACL permission matrix: rows Owner, Authenticated Users, All Users;
/// columns Read, Write, Read ACP, Write ACP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AclMatrix {
    /// Owner row.
    pub owner: [bool; 4],
    /// Authenticated users row.
    pub authenticated: [bool; 4],
    /// Everyone row.
    pub everyone: [bool; 4],
}

impl Acl {
    /// Matrix view plus grants that do not fit it (other users, e-mail grantees).
    pub fn matrix(&self) -> (AclMatrix, Vec<AclGrant>) {
        let mut m = AclMatrix::default();
        let mut others = Vec::new();
        for g in &self.grants {
            let row = if g.kind == "CanonicalUser" && g.grantee == self.owner_id {
                Some(&mut m.owner)
            } else if g.kind == "Group" && g.grantee == AUTHENTICATED_USERS {
                Some(&mut m.authenticated)
            } else if g.kind == "Group" && g.grantee == ALL_USERS {
                Some(&mut m.everyone)
            } else {
                None
            };
            match (row, g.permission.as_str()) {
                (Some(row), "FULL_CONTROL") => *row = [true; 4],
                (Some(row), p) => match PERMISSIONS.iter().position(|x| *x == p) {
                    Some(i) => row[i] = true,
                    None => others.push(g.clone()),
                },
                (None, _) => others.push(g.clone()),
            }
        }
        (m, others)
    }

    /// Rebuilds an ACL from the matrix, keeping the other grants unchanged.
    pub fn from_matrix(
        owner_id: &str,
        owner_name: Option<String>,
        m: &AclMatrix,
        others: &[AclGrant],
    ) -> Self {
        let mut grants = Vec::new();
        let mut row = |kind: &str, grantee: &str, name: Option<String>, perms: &[bool; 4]| {
            if perms.iter().all(|p| *p) {
                grants.push(AclGrant {
                    kind: kind.into(),
                    grantee: grantee.into(),
                    display_name: name,
                    permission: "FULL_CONTROL".into(),
                });
                return;
            }
            for (i, on) in perms.iter().enumerate() {
                if *on {
                    grants.push(AclGrant {
                        kind: kind.into(),
                        grantee: grantee.into(),
                        display_name: name.clone(),
                        permission: PERMISSIONS[i].into(),
                    });
                }
            }
        };
        row("CanonicalUser", owner_id, owner_name.clone(), &m.owner);
        row("Group", AUTHENTICATED_USERS, None, &m.authenticated);
        row("Group", ALL_USERS, None, &m.everyone);
        grants.extend(others.iter().cloned());
        Self {
            owner_id: owner_id.into(),
            owner_name,
            grants,
        }
    }
}

/// One CORS rule.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct CorsRule {
    /// Origins (`*` allowed).
    pub allowed_origins: Vec<String>,
    /// HTTP methods: GET, PUT, POST, DELETE, HEAD.
    pub allowed_methods: Vec<String>,
    /// Request headers.
    pub allowed_headers: Vec<String>,
    /// Response headers exposed to scripts.
    pub expose_headers: Vec<String>,
    /// Preflight cache time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_age_seconds: Option<i32>,
}

/// One lifecycle rule (the subset the bucket settings dialog edits).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LifecycleRule {
    /// Rule name.
    pub id: String,
    /// Enabled or disabled.
    pub enabled: bool,
    /// Key prefix the rule applies to (`""` = whole bucket).
    pub prefix: String,
    /// Delete current versions after this many days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration_days: Option<i32>,
    /// Delete noncurrent versions after this many days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noncurrent_expiration_days: Option<i32>,
    /// Abort unfinished multipart uploads after this many days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_incomplete_upload_days: Option<i32>,
    /// Move to another storage class after this many days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition_days: Option<i32>,
    /// Target storage class of the transition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition_storage_class: Option<String>,
}

/// Default bucket encryption.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Encryption {
    /// No default encryption configured.
    #[default]
    None,
    /// SSE-S3 (AES-256, keys managed by the server).
    Aes256,
    /// SSE-KMS with this key id or ARN (empty = the service default key).
    Kms(String),
}

const METHODS: &[&str] = &["GET", "PUT", "POST", "DELETE", "HEAD"];

/// Parses and checks CORS rules written as JSON (an array of rules).
pub fn parse_cors(text: &str) -> Result<Vec<CorsRule>, String> {
    let rules: Vec<CorsRule> =
        serde_json::from_str(text).map_err(|e| format!("CORS rules are not valid JSON: {e}"))?;
    if rules.len() > 100 {
        return Err("At most 100 CORS rules are allowed.".into());
    }
    for (i, r) in rules.iter().enumerate() {
        let n = i + 1;
        if r.allowed_origins.is_empty() {
            return Err(format!("Rule {n}: AllowedOrigins must not be empty."));
        }
        if r.allowed_methods.is_empty() {
            return Err(format!("Rule {n}: AllowedMethods must not be empty."));
        }
        if let Some(m) = r
            .allowed_methods
            .iter()
            .find(|m| !METHODS.contains(&m.to_ascii_uppercase().as_str()))
        {
            return Err(format!(
                "Rule {n}: \"{m}\" is not an allowed method (GET, PUT, POST, DELETE, HEAD)."
            ));
        }
        if r.max_age_seconds.is_some_and(|s| s < 0) {
            return Err(format!("Rule {n}: MaxAgeSeconds must not be negative."));
        }
    }
    Ok(rules)
}

/// Parses and checks lifecycle rules written as JSON (an array of rules).
pub fn parse_lifecycle(text: &str) -> Result<Vec<LifecycleRule>, String> {
    let rules: Vec<LifecycleRule> = serde_json::from_str(text)
        .map_err(|e| format!("Lifecycle rules are not valid JSON: {e}"))?;
    if rules.len() > 1000 {
        return Err("At most 1000 lifecycle rules are allowed.".into());
    }
    let mut ids = std::collections::HashSet::new();
    for (i, r) in rules.iter().enumerate() {
        let n = i + 1;
        if r.id.trim().is_empty() || r.id.len() > 255 {
            return Err(format!("Rule {n}: Id is required (max 255 characters)."));
        }
        if !ids.insert(r.id.clone()) {
            return Err(format!("Rule {n}: Id \"{}\" is used twice.", r.id));
        }
        let days = [
            r.expiration_days,
            r.noncurrent_expiration_days,
            r.abort_incomplete_upload_days,
            r.transition_days,
        ];
        if days.iter().all(Option::is_none) {
            return Err(format!(
                "Rule {n}: set at least one action (expiration, noncurrent expiration, abort uploads or transition)."
            ));
        }
        if days.iter().flatten().any(|d| *d <= 0) {
            return Err(format!("Rule {n}: days must be greater than 0."));
        }
        if r.transition_days.is_some() != r.transition_storage_class.is_some() {
            return Err(format!(
                "Rule {n}: a transition needs both TransitionDays and TransitionStorageClass."
            ));
        }
    }
    Ok(rules)
}

/// Checks a bucket policy document (JSON object with a `Statement`). Returns it re-indented.
pub fn check_policy(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("The policy is not valid JSON: {e}"))?;
    let statement = value
        .get("Statement")
        .ok_or("The policy needs a \"Statement\" element.")?;
    if !(statement.is_array() || statement.is_object()) {
        return Err("\"Statement\" must be an object or an array.".into());
    }
    if text.len() > 20 * 1024 {
        return Err("Bucket policies are limited to 20 KB.".into());
    }
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// Pretty JSON of rules for the editors.
pub fn to_json<T: Serialize>(rules: &[T]) -> String {
    serde_json::to_string_pretty(rules).unwrap_or_else(|_| "[]".into())
}

/// Starting text for an empty CORS editor.
pub const CORS_TEMPLATE: &str = r#"[
  {
    "AllowedOrigins": ["https://example.com"],
    "AllowedMethods": ["GET", "HEAD"],
    "AllowedHeaders": ["*"],
    "ExposeHeaders": ["ETag"],
    "MaxAgeSeconds": 3000
  }
]"#;

/// Starting text for an empty lifecycle editor.
pub const LIFECYCLE_TEMPLATE: &str = r#"[
  {
    "Id": "expire-temp",
    "Enabled": true,
    "Prefix": "tmp/",
    "ExpirationDays": 30,
    "AbortIncompleteUploadDays": 7
  }
]"#;

/// Starting text for an empty policy editor (public read of one prefix).
pub fn policy_template(bucket: &str) -> String {
    format!(
        r#"{{
  "Version": "2012-10-17",
  "Statement": [
    {{
      "Sid": "PublicReadForPublicFolder",
      "Effect": "Allow",
      "Principal": "*",
      "Action": ["s3:GetObject"],
      "Resource": ["arn:aws:s3:::{bucket}/public/*"]
    }}
  ]
}}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(kind: &str, grantee: &str, permission: &str) -> AclGrant {
        AclGrant {
            kind: kind.into(),
            grantee: grantee.into(),
            display_name: None,
            permission: permission.into(),
        }
    }

    #[test]
    fn acl_matrix_round_trip() {
        let acl = Acl {
            owner_id: "own".into(),
            owner_name: Some("me".into()),
            grants: vec![
                AclGrant {
                    display_name: Some("me".into()),
                    ..grant("CanonicalUser", "own", "FULL_CONTROL")
                },
                grant("Group", ALL_USERS, "READ"),
                grant("CanonicalUser", "someone", "READ"),
            ],
        };
        let (m, others) = acl.matrix();
        assert_eq!(m.owner, [true; 4]);
        assert_eq!(m.everyone, [true, false, false, false]);
        assert_eq!(m.authenticated, [false; 4]);
        assert_eq!(others.len(), 1);
        let rebuilt = Acl::from_matrix("own", Some("me".into()), &m, &others);
        assert_eq!(rebuilt.grants, acl.grants);
        let mut m2 = m;
        m2.everyone = [false; 4];
        m2.authenticated = [true, false, true, false];
        let r = Acl::from_matrix("own", None, &m2, &[]);
        let perms: Vec<_> = r
            .grants
            .iter()
            .map(|g| (g.grantee.as_str(), g.permission.as_str()))
            .collect();
        assert_eq!(
            perms,
            [
                ("own", "FULL_CONTROL"),
                (AUTHENTICATED_USERS, "READ"),
                (AUTHENTICATED_USERS, "READ_ACP")
            ]
        );
    }

    #[test]
    fn cors_validation() {
        assert_eq!(parse_cors(CORS_TEMPLATE).expect("template").len(), 1);
        assert!(parse_cors("[]").expect("empty").is_empty());
        assert!(parse_cors("{").unwrap_err().contains("JSON"));
        assert!(
            parse_cors(r#"[{"AllowedMethods":["GET"]}]"#)
                .unwrap_err()
                .contains("Origins")
        );
        assert!(
            parse_cors(r#"[{"AllowedOrigins":["*"],"AllowedMethods":["PATCH"]}]"#)
                .unwrap_err()
                .contains("PATCH")
        );
    }

    #[test]
    fn lifecycle_validation() {
        assert_eq!(
            parse_lifecycle(LIFECYCLE_TEMPLATE).expect("template")[0].expiration_days,
            Some(30)
        );
        assert!(
            parse_lifecycle(r#"[{"Id":"a","Enabled":true}]"#)
                .unwrap_err()
                .contains("at least one action")
        );
        assert!(
            parse_lifecycle(r#"[{"Id":"a","ExpirationDays":0}]"#)
                .unwrap_err()
                .contains("greater than 0")
        );
        assert!(
            parse_lifecycle(r#"[{"Id":"a","TransitionDays":30}]"#)
                .unwrap_err()
                .contains("both")
        );
        assert!(
            parse_lifecycle(r#"[{"Id":"a","ExpirationDays":1},{"Id":"a","ExpirationDays":2}]"#)
                .unwrap_err()
                .contains("twice")
        );
    }

    #[test]
    fn policy_validation() {
        assert!(
            check_policy(&policy_template("b"))
                .expect("template")
                .contains("arn:aws:s3:::b/public/*")
        );
        assert!(check_policy("{}").unwrap_err().contains("Statement"));
        assert!(check_policy("nope").unwrap_err().contains("JSON"));
    }
}
