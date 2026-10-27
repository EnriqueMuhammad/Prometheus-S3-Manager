//! Embeds the icon and version information (product name, author) into the Windows `.exe`.

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-env-changed=S3M_UPDATE_REPO");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico")
            .set("ProductName", "Prometheus S3Manager")
            .set("FileDescription", "Prometheus S3Manager")
            .set("CompanyName", "Enrique Muhammad Ilham")
            .set(
                "LegalCopyright",
                "Copyright (c) 2026 Enrique Muhammad Ilham",
            )
            .set("OriginalFilename", "s3manager.exe")
            .set("InternalName", "s3manager");
        if let Err(e) = res.compile() {
            // Missing Windows SDK resource compiler: build without icon/metadata.
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
