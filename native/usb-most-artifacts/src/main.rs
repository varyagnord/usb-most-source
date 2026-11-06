//! Build-time only, never linked into APK/receiver. Extract signed vendor files
//! without executing any installer logic or modifying the builder's OS.
#![forbid(unsafe_code)]
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::PathBuf,
};
const NAMES: [&str; 6] = [
    "usbip2_ude.inf",
    "usbip2_ude.sys",
    "usbip2_ude.cat",
    "usbip2_filter.inf",
    "usbip2_filter.sys",
    "usbip2_filter.cat",
];
fn hash(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: usb-most-artifacts VENDOR_EXE OUTPUT_UNDER_BUILD".into());
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let source = PathBuf::from(&args[0]);
    let file = fs::File::open(&source)?;
    let len = file.metadata()?.len();
    if len > 27 * 1024 * 1024 {
        return Err(invalid().into());
    }
    let mut bytes = Vec::new();
    file.take(27 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    let digest = hash(&bytes);
    let arch = match (len, digest.as_str()) {
        (26083632, "38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a") => "x86_64",
        (19060408, "cab7ff97f79275eeb5c5b8bb2eb3111ff6b6c4eead07d9bec9be5bd1e3a35800") => "aarch64",
        _ => return Err("vendor distribution hash is not pinned".into()),
    };
    let output = PathBuf::from(&args[1]);
    fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    if !output.starts_with(root.join("build")) {
        return Err("artifact output must be inside project build/".into());
    }
    let mut installer = inno::Inno::new(std::io::Cursor::new(bytes))?;
    let mut files = installer.streaming_files(|entry| {
        entry
            .file()
            .destination()
            .is_some_and(|s| NAMES.contains(&s.rsplit(['\\', '/']).next().unwrap_or("")))
    });
    let mut entries = BTreeMap::new();
    while let Some(result) = files.next() {
        let (entry, reader) = result?;
        let name = entry
            .file()
            .destination()
            .unwrap()
            .rsplit(['\\', '/'])
            .next()
            .unwrap();
        if entries.contains_key(name) {
            return Err("duplicate signed driver filename".into());
        }
        let mut data = Vec::new();
        reader.take(2 * 1024 * 1024 + 1).read_to_end(&mut data)?;
        if data.is_empty() || data.len() > 2 * 1024 * 1024 {
            return Err(invalid().into());
        }
        let target = output.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(mut f) => {
                f.write_all(&data)?;
                f.sync_all()?
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let m = fs::symlink_metadata(&target)?;
                if !m.is_file()
                    || m.file_type().is_symlink()
                    || m.len() != data.len() as u64
                    || fs::read(&target)? != data
                {
                    return Err("existing artifact differs; retained for inspection".into());
                }
            }
            Err(e) => return Err(e.into()),
        }
        entries.insert(
            name.to_owned(),
            serde_json::json!({"bytes":data.len(),"sha256":hash(&data)}),
        );
    }
    if entries.len() != NAMES.len() {
        return Err("incomplete signed driver package".into());
    }
    let manifest = serde_json::json!({"schema":1,"vendor":"usbip-win2","version":"0.9.8.1","source_sha256":digest,"source_bytes":len,"arch":arch,"files":entries});
    let encoded = serde_json::to_vec_pretty(&manifest)?;
    let target = output.join("manifest.json");
    if target.exists() {
        if fs::symlink_metadata(&target)?.file_type().is_symlink() || fs::read(&target)? != encoded
        {
            return Err("existing artifact manifest differs".into());
        }
    } else {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        f.write_all(&encoded)?;
        f.sync_all()?
    }
    println!(
        "Verified/extracted {} unchanged signed driver files, {arch}",
        entries.len()
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("USB Most artifact build: {error}");
        std::process::exit(1)
    }
}
