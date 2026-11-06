//! The desktop user opens the browser. Only the fixed setup child is elevated
//! through the system's PolicyKit agent. Capabilities travel over inherited
//! stdout and an owner-only file, never shell strings or process arguments.
use std::{
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use zeroize::Zeroizing;
fn valid_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("http://127.0.0.1:") else {
        return false;
    };
    let Some((port, token)) = rest.split_once("/#") else {
        return false;
    };
    port.parse::<u16>().is_ok_and(|p| p >= 1024)
        && token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
async fn read_url(mut stream: impl AsyncRead + Unpin) -> io::Result<Zeroizing<String>> {
    let mut line = Zeroizing::new(Vec::new());
    for _ in 0..2048 {
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            let value = std::str::from_utf8(&line).map_err(|_| super::invalid())?;
            if valid_url(value) {
                return Ok(Zeroizing::new(value.to_owned()));
            }
            line.clear();
        } else {
            line.push(byte);
        }
    }
    Err(super::invalid())
}
struct Handoff {
    directory: PathBuf,
    file: PathBuf,
}
impl Handoff {
    fn create(url: &str) -> io::Result<Self> {
        if !valid_url(url) {
            return Err(super::invalid());
        }
        let nonce = usb_most_connection::security::random::<16>().map_err(|_| super::invalid())?;
        let directory =
            std::env::temp_dir().join(format!("usb-most-ui-{}", super::super::digest_hex(&nonce)));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let file = directory.join("open.html");
        // Create-new inside a fresh owner-only directory; do not follow a link.
        let mut output = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&file)
        {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_dir(&directory);
                return Err(error);
            }
        };
        let handoff = Self { directory, file };
        let html = Zeroizing::new(format!(
            "<!doctype html><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"refresh\" content=\"0;url={url}\"><title>USB Мост</title><a href=\"{url}\">Открыть USB Мост</a>"
        ));
        output.write_all(html.as_bytes())?;
        output.sync_all()?;
        Ok(handoff)
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_dir(&self.directory);
    }
}
pub async fn launch() -> io::Result<()> {
    if !std::path::Path::new("/usr/bin/pkexec").is_file()
        || !std::path::Path::new("/usr/bin/xdg-open").is_file()
    {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut helper = tokio::process::Command::new("/usr/bin/pkexec")
        .arg(std::env::current_exe()?)
        .arg("--setup-elevated")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdout = helper.stdout.take().ok_or_else(super::invalid)?;
    let url = tokio::time::timeout(Duration::from_secs(180), read_url(stdout))
        .await
        .map_err(|_| io::ErrorKind::TimedOut)??;
    let handoff = Handoff::create(&url)?;
    let mut browser = tokio::process::Command::new("/usr/bin/xdg-open")
        .arg(&handoff.file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // A browser may keep its launcher alive; never kill the browser on timeout.
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), browser.wait()).await
        && !result?.success()
    {
        return Err(io::ErrorKind::NotFound.into());
    }
    // The privileged child expires its own UI after30min. The service survives.
    let status = tokio::time::timeout(Duration::from_secs(1980), helper.wait())
        .await
        .map_err(|_| io::ErrorKind::TimedOut)??;
    if status.success() {
        Ok(())
    } else {
        Err(io::ErrorKind::Other.into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn handoff_accepts_only_bounded_generated_loopback_capabilities() {
        let url = format!("http://127.0.0.1:43210/#{}", "X".repeat(43));
        assert!(valid_url(&url));
        for bad in [
            url.replace("127.0.0.1", "example.com"),
            url.replace("43210", "0"),
            url.replace("43210", "65536"),
            url.replace("http:", "file:"),
            format!("{url}\""),
            format!("{url}/"),
            "http://127.0.0.1:43210/#x".into(),
        ] {
            assert!(!valid_url(&bad));
        }
        let data = format!("USB Мост\n{url}\n");
        assert_eq!(read_url(data.as_bytes()).await.unwrap().as_str(), url);
        assert!(read_url(&b"truncated"[..]).await.is_err());
        assert!(read_url(vec![b'x'; 2049].as_slice()).await.is_err());
        let handoff = Handoff::create(&url).unwrap();
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory = std::fs::metadata(&handoff.directory).unwrap();
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        assert_eq!(directory.uid(), rustix::process::geteuid().as_raw());
        assert_eq!(
            std::fs::metadata(&handoff.file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let file = handoff.file.clone();
        let dir = handoff.directory.clone();
        std::fs::write(dir.join("unexpected"), b"retain").unwrap();
        drop(handoff);
        assert!(!file.exists());
        assert_eq!(std::fs::read(dir.join("unexpected")).unwrap(), b"retain");
        std::fs::remove_file(dir.join("unexpected")).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
