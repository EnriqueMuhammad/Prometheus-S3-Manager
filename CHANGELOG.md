# Changelog

All notable changes to Prometheus S3Manager. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## 0.1.0 - Unreleased

First release.

### Added

- **Accounts**: any number of accounts and keys, folders, provider presets, test connection,
  import/export with a password. Secret keys live in the Windows Credential Manager, optionally
  protected by a master password.
- **Secure connections**: TLS with the Windows certificate store (company CAs work) and an
  optional custom CA per account. Plain HTTP only for private-network addresses.
- **Files**: browse, filter and sort; upload files and folders (drag and drop from Explorer),
  download, rename, delete, new folder, copy/cut/paste between folders, buckets and accounts.
- **Transfers**: queue with retry, pause/resume (also after a restart), multipart uploads,
  bandwidth limit, speed and time left.
- **Objects**: properties, headers and metadata editor, preview of text and images, versions
  (download, restore, delete), permissions (ACL), presigned web URLs.
- **Buckets**: create, delete, properties, and settings for versioning, encryption, policy, CORS,
  lifecycle and ACL.
- **Access keys (MinIO)**: list, create, edit, enable/disable and delete the access keys
  (service accounts) of your user, with an optional JSON policy, expiry, name and description.
  A new key's credentials are shown once and can be added as a storage account right away.
- **Sync**: compare a local folder with a bucket folder and upload or download the differences.
- **Proxy**: none, Windows settings, or a manual proxy with authentication.
- **Light and dark themes**: sun/moon button at the right of the menu bar and Tools > Dark
  theme; the choice is remembered. Text, borders and buttons are high-contrast in both themes
  (WCAG AA or better), on softened backgrounds that do not glare.
- **Updates**: the app checks GitHub Releases for new versions and installs them on request
  (checksum-verified per-user MSI, no administrator rights).
- **Installer**: per-user MSI with a welcome page, license, progress and a "Launch Prometheus
  S3Manager" finish page; an installed version offers Repair and Remove. A portable `.exe` is
  published too.

### Window and performance

- The window opens centered on the primary monitor with its last size (or maximized); its
  position is not remembered, so it no longer opens off screen or at twice its size on monitors
  with different scale factors. A window too large for the screen opens maximized and restores
  to a size that fits; after that, resizing, snapping and dragging an edge are never undone.
- Transfer progress updates ten times a second while transfers run.
- Text previews draw only the visible lines, so large files scroll smoothly.
