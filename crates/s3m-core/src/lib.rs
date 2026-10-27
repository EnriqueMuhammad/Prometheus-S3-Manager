//! Domain layer of Prometheus S3Manager.
//!
//! Nothing here knows about egui or the AWS SDK: accounts and their validation, endpoint and
//! private-network rules, local path safety, the account export format and the transfer queue.

pub mod account;
pub mod admin;
pub mod browse;
pub mod bucket;
pub mod ca;
pub mod endpoint;
pub mod export;
pub mod format;
pub mod minio;
pub mod network;
pub mod paths;
pub mod plan;
pub mod presets;
pub mod sync;
pub mod transfer;
pub mod update;
pub mod uploads;
pub mod vault;

pub use account::{
    Account, AccountDraft, AccountError, AccountRepository, AccountService, AccountType,
    AccountsDocument, SecretError, SecretStore,
};
pub use browse::{
    BucketEntry, BucketInfo, BucketListing, ConnectionTestResult, HeaderEdit, ObjectDetails,
    ObjectEntry, ObjectPage, ObjectVersion,
};
pub use transfer::{
    ObjectRef, TransferDirection, TransferError, TransferExecutor, TransferJob, TransferOptions,
    TransferQueue, TransferRequest, TransferState, TransferStore,
};

/// Product name shown in titles and dialogs.
pub const APP_NAME: &str = "Prometheus S3Manager";

/// Boxed `Send` future used by object-safe async traits.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
