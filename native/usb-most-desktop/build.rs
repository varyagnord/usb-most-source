//! Embeds only the six pinned signed driver files, never the vendor installer.
use std::{env, fs, path::PathBuf};
#[path = "src/local_ui/listing.rs"]
mod listing;
fn main() {
    println!("cargo:rerun-if-changed=src/local_ui/rustore.json");
    println!("cargo:rerun-if-changed=src/local_ui/listing.rs");
    listing::parse(include_str!("src/local_ui/rustore.json"))
        .expect("Invalid public RuStore listing configuration");
    println!("cargo:rerun-if-env-changed=USB_MOST_BUNDLED_DRIVER_DIR");
    if env::var_os("CARGO_FEATURE_BUNDLED_DRIVER").is_none() {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_OS").unwrap(),
        "windows",
        "driver bundle is Windows-only"
    );
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
        "x86_64",
        "only the independently pinned x64 package is currently released"
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../..")
        .canonicalize()
        .unwrap();
    let source = PathBuf::from(
        env::var_os("USB_MOST_BUNDLED_DRIVER_DIR")
            .expect("verified extracted driver directory required"),
    )
    .canonicalize()
    .unwrap();
    assert!(
        source.starts_with(root.join("build")),
        "driver files must be in the build workspace"
    );
    let manifest = root.join("server/windows-driver-lock-x86_64.json");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let files = manifest["files"].as_object().unwrap();
    assert_eq!(files.len(), 6);
    let mut generated = String::from("const FILES: &[(&str, &str, &[u8])] = &[\n");
    for name in [
        "usbip2_ude.inf",
        "usbip2_ude.sys",
        "usbip2_ude.cat",
        "usbip2_filter.inf",
        "usbip2_filter.sys",
        "usbip2_filter.cat",
    ] {
        let path = source.join(name);
        assert!(fs::symlink_metadata(&path).unwrap().is_file());
        let data = fs::read(&path).unwrap();
        assert!(data.len() <= 1024 * 1024);
        assert_eq!(data.len() as u64, files[name]["bytes"].as_u64().unwrap());
        let hash = ring::digest::digest(&ring::digest::SHA256, &data)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(hash, files[name]["sha256"].as_str().unwrap());
        println!("cargo:rerun-if-changed={}", path.display());
        generated += &format!("({name:?}, {hash:?}, include_bytes!({path:?})),\n");
    }
    generated += "];\n";
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("driver-bundle.rs"),
        generated,
    )
    .unwrap();
}
