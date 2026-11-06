//! Fixed-code preflight over the authenticated SSH session. Variable input is stdin.
use crate::{provision::Probe, ssh::SshClient};
use base64::Engine;
use std::{sync::LazyLock, time::Duration};
use zeroize::Zeroizing;
pub const LINUX_PROBE: &str = include_str!("bootstrap/probe-linux.sh");
pub const WINDOWS_PROBE: &str = concat!(
    include_str!("bootstrap/protection-windows.ps1"),
    include_str!("bootstrap/probe-windows.ps1")
);
static LINUX: LazyLock<String> = LazyLock::new(|| {
    format!(
        "sh -c '{}' usb-most-probe",
        LINUX_PROBE.replace('\'', "'\\''")
    )
});
// Default Windows OpenSSH uses cmd.exe (8191-character command limit).
// UTF8/base64 avoids UTF16 expansion; only our immutable script is compiled.
// Configuration/passwords/artifacts remain stdin data, never ScriptBlock input.
fn powershell(script: &str) -> String {
    format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"& ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))))\"",
        base64::engine::general_purpose::STANDARD.encode(script.as_bytes())
    )
}
static WINDOWS: LazyLock<String> = LazyLock::new(|| powershell(WINDOWS_PROBE));
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Transport,
    Unsupported,
    InvalidResponse,
    RestartRequired,
}
pub fn linux_probe_command() -> &'static str {
    &LINUX
}
pub fn windows_probe_command() -> &'static str {
    &WINDOWS
}
pub async fn probe(client: &SshClient) -> Result<Probe, Error> {
    let random = crate::security::random::<16>().map_err(|_| Error::Transport)?;
    let nonce = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let input = || Zeroizing::new(format!("{nonce}\n").into_bytes());
    // A multiline POSIX program is not a safe shell discriminator: cmd.exe
    // interprets its redirects/operators before sh sees them. Start with one
    // fixed, argument-free, read-only command understood by POSIX/Git shells.
    let platform = client
        .exec(
            "uname -s",
            Zeroizing::new(Vec::new()),
            Duration::from_secs(20),
        )
        .await
        .map_err(|_| Error::Transport)?;
    let command = select_probe(platform.exit, &platform.stdout)?;
    let response = client
        .exec(command, input(), Duration::from_secs(20))
        .await
        .map_err(|_| Error::Transport)?;
    if response.exit != 0 {
        return Err(Error::Unsupported);
    }
    Probe::parse(&response.stdout, &nonce).map_err(|_| Error::InvalidResponse)
}

fn select_probe(exit: u32, stdout: &[u8]) -> Result<&'static str, Error> {
    let name = std::str::from_utf8(stdout)
        .map_err(|_| Error::InvalidResponse)?
        .trim();
    if exit == 0 && name == "Linux" {
        Ok(linux_probe_command())
    } else if (exit == 0
        && ["MINGW", "MSYS", "CYGWIN"]
            .iter()
            .any(|prefix| name.starts_with(prefix)))
        || ([1, 127, 9009].contains(&exit) && name.is_empty())
    {
        Ok(windows_probe_command())
    } else {
        Err(Error::Unsupported)
    }
}

const LINUX_INSTALL: &str = include_str!("bootstrap/install-linux.sh");
const LINUX_CONFIGURE: &str = include_str!("bootstrap/configure-linux.sh");
static LINUX_CONFIGURE_ROOT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "sh -c '{}' usb-most-configure",
        LINUX_CONFIGURE.replace('\'', "'\\''")
    )
});
static LINUX_CONFIGURE_SUDO: LazyLock<String> = LazyLock::new(|| {
    format!(
        "sudo -S -p '' -- sh -c '{}' usb-most-configure",
        LINUX_CONFIGURE.replace('\'', "'\\''")
    )
});
static LINUX_INSTALL_ROOT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "sh -c '{}' usb-most-install",
        LINUX_INSTALL.replace('\'', "'\\''")
    )
});
static LINUX_INSTALL_SUDO: LazyLock<String> = LazyLock::new(|| {
    format!(
        "sudo -S -p '' -- sh -c '{}' usb-most-install",
        LINUX_INSTALL.replace('\'', "'\\''")
    )
});
/// Called with the artifact selected from the trusted APK manifest. No remote
/// response supplies a command, URL, path or executable hash.
pub async fn install_linux(
    client: &SshClient,
    probe: &Probe,
    binary: &[u8],
    expected_hash: &str,
    config: &[u8],
    sudo_password: Option<&str>,
) -> Result<(), Error> {
    if probe.os != crate::provision::Os::Linux
        || binary.is_empty()
        || binary.len() > 20 * 1024 * 1024
        || config.len() > 4096
    {
        return Err(Error::Unsupported);
    }
    let actual = ring::digest::digest(&ring::digest::SHA256, binary)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if actual != expected_hash {
        return Err(Error::InvalidResponse);
    }
    let mut prefix = Zeroizing::new(Vec::new());
    if !probe.elevated {
        let password = sudo_password.unwrap_or("");
        if password.len() > 4096 || password.contains(['\n', '\r', '\0']) {
            return Err(Error::Unsupported);
        }
        prefix.extend_from_slice(password.as_bytes());
        prefix.push(b'\n');
    }
    let encoded_config = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(config));
    if probe.agent_version.as_deref() == Some(env!("CARGO_PKG_VERSION"))
        && probe.agent_sha256.as_deref() == Some(expected_hash)
    {
        let mut payload = Zeroizing::new(prefix.to_vec());
        payload.extend_from_slice(format!("UMCONF1\n{expected_hash}\n").as_bytes());
        payload.extend_from_slice(encoded_config.as_bytes());
        payload.push(b'\n');
        let command = if probe.elevated {
            LINUX_CONFIGURE_ROOT.as_str()
        } else {
            LINUX_CONFIGURE_SUDO.as_str()
        };
        let result = client
            .exec(command, payload, Duration::from_secs(900))
            .await
            .map_err(|_| Error::Transport)?;
        match result.exit {
            0 => return Ok(()),
            // File disappeared/changed after the advisory probe. The root
            // helper guarantees this status precedes any mutation/exec.
            78 => {}
            76 => return Err(Error::RestartRequired),
            _ => return Err(Error::InvalidResponse),
        }
    }
    let mut payload = prefix;
    payload.extend_from_slice(format!("UMBOOT1\n{expected_hash}\n").as_bytes());
    payload.extend_from_slice(
        base64::engine::general_purpose::STANDARD
            .encode(binary)
            .as_bytes(),
    );
    payload.push(b'\n');
    payload.extend_from_slice(encoded_config.as_bytes());
    payload.push(b'\n');
    let command = if probe.elevated {
        LINUX_INSTALL_ROOT.as_str()
    } else {
        LINUX_INSTALL_SUDO.as_str()
    };
    let result = client
        .exec(command, payload, Duration::from_secs(900))
        .await
        .map_err(|_| Error::Transport)?;
    if result.exit != 0 {
        return Err(Error::InvalidResponse);
    }
    Ok(())
}

const WINDOWS_CONFIGURE: &str = concat!(
    include_str!("bootstrap/protection-windows.ps1"),
    include_str!("bootstrap/configure-windows.ps1")
);
static WINDOWS_CONFIGURE_COMMAND: LazyLock<String> =
    LazyLock::new(|| powershell(WINDOWS_CONFIGURE));
const WINDOWS_INSTALL: &str = concat!(
    include_str!("bootstrap/protection-windows.ps1"),
    include_str!("bootstrap/install-windows.ps1")
);
static WINDOWS_INSTALL_COMMAND: LazyLock<String> = LazyLock::new(|| powershell(WINDOWS_INSTALL));
pub fn windows_install_command() -> &'static str {
    &WINDOWS_INSTALL_COMMAND
}
pub fn windows_configure_command() -> &'static str {
    &WINDOWS_CONFIGURE_COMMAND
}
/// The embedded Windows receiver carries its independently pinned signed driver.
/// Non-administrative SSH is reported; no hidden elevation prompt/password relay.
pub async fn install_windows(
    client: &SshClient,
    probe: &Probe,
    binary: &[u8],
    expected_hash: &str,
    config: &[u8],
) -> Result<(), Error> {
    if probe.os != crate::provision::Os::Windows
        || !probe.elevated
        || binary.is_empty()
        || binary.len() > 20 * 1024 * 1024
        || config.len() > 4096
    {
        return Err(Error::Unsupported);
    }
    let actual = ring::digest::digest(&ring::digest::SHA256, binary)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if actual != expected_hash {
        return Err(Error::InvalidResponse);
    }
    if probe.agent_version.as_deref() == Some(env!("CARGO_PKG_VERSION"))
        && probe.agent_sha256.as_deref() == Some(expected_hash)
    {
        let encoded = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(config));
        let payload = Zeroizing::new(
            format!("UMCONF1\n{expected_hash}\n{}\n", encoded.as_str()).into_bytes(),
        );
        let result = client
            .exec(
                &WINDOWS_CONFIGURE_COMMAND,
                payload,
                Duration::from_secs(900),
            )
            .await
            .map_err(|_| Error::Transport)?;
        match result.exit {
            0 => return Ok(()),
            78 => {}
            76 => return Err(Error::RestartRequired),
            _ => return Err(Error::InvalidResponse),
        }
    }
    let mut payload = Zeroizing::new(format!("UMBOOT1\n{expected_hash}\n").into_bytes());
    payload.extend_from_slice(
        base64::engine::general_purpose::STANDARD
            .encode(binary)
            .as_bytes(),
    );
    payload.push(b'\n');
    let encoded = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(config));
    payload.extend_from_slice(encoded.as_bytes());
    payload.push(b'\n');
    let result = client
        .exec(&WINDOWS_INSTALL_COMMAND, payload, Duration::from_secs(900))
        .await
        .map_err(|_| Error::Transport)?;
    match result.exit {
        0 => Ok(()),
        76 => Err(Error::RestartRequired),
        _ => Err(Error::InvalidResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_selection_never_sends_posix_code_to_an_unknown_shell() {
        assert_eq!(
            select_probe(0, b"Linux\r\n").unwrap(),
            linux_probe_command()
        );
        for (exit, value) in [
            (0, "MINGW64_NT-10.0"),
            (0, "MSYS_NT-10.0"),
            (0, "CYGWIN_NT"),
            (1, ""),
            (127, ""),
            (9009, ""),
        ] {
            assert_eq!(
                select_probe(exit, value.as_bytes()).unwrap(),
                windows_probe_command()
            );
        }
        for (exit, value) in [
            (0, "Darwin"),
            (0, "FreeBSD"),
            (0, ""),
            (0, "Linux\nextra"),
            (2, ""),
            (1, "unexpected"),
            (255, ""),
        ] {
            assert!(select_probe(exit, value.as_bytes()).is_err());
        }
        assert!(select_probe(0, &[0xff]).is_err());
    }
    #[test]
    fn encoded_command_contains_only_fixed_script_and_no_runtime_configuration() {
        for (script, command) in [
            (WINDOWS_PROBE, windows_probe_command()),
            (WINDOWS_INSTALL, WINDOWS_INSTALL_COMMAND.as_str()),
            (WINDOWS_CONFIGURE, WINDOWS_CONFIGURE_COMMAND.as_str()),
        ] {
            assert!(command.len() < 8191, "Windows cmd.exe length limit");
            let encoded = command
                .split("FromBase64String('")
                .nth(1)
                .unwrap()
                .split("')")
                .next()
                .unwrap();
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap(),
                script.as_bytes()
            );
        }
        assert!(LINUX_PROBE.contains("read -r nonce"));
        assert!(!LINUX_PROBE.contains("eval "));
        assert!(!windows_probe_command().contains("ExecutionPolicy"));
    }
}
