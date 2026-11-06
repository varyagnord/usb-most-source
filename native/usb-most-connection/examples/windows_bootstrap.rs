//! Actual cmd.exe -> Windows PowerShell 5.1 -> pinned bundle -> SCM.
//! Runs only in an explicitly isolated VM; no physical USB passthrough.
#[cfg(not(windows))]
fn main() {}
#[cfg(windows)]
fn main() {
    use base64::Engine;
    use std::{
        fs,
        io::Write,
        net::TcpListener,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use usb_most_connection::{bootstrap, provision::Probe};
    assert!(
        std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
            || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
    );
    let binary_path = std::env::args().nth(1).expect("bundled receiver path");
    let version = Command::new(&binary_path)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).unwrap();
    let version = version.trim();
    assert!(!version.is_empty() && version.len() <= 32);
    let binary = fs::read(&binary_path).unwrap();
    assert!(binary.len() <= 20 * 1024 * 1024);
    let hash = ring::digest::digest(&ring::digest::SHA256, &binary)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    fn execute(command: &str, data: &[u8], exit: i32) -> Vec<u8> {
        assert!(command.len() < 8191);
        use std::os::windows::process::CommandExt;
        let mut child = Command::new("cmd.exe")
            .args(["/d", "/s", "/c"])
            .raw_arg(format!("\"{command}\""))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let stdout = std::thread::spawn(move || {
            use std::io::Read;
            let mut b = vec![];
            out.take(16385).read_to_end(&mut b).unwrap();
            b
        });
        let stderr = std::thread::spawn(move || {
            use std::io::Read;
            let mut b = vec![];
            err.take(16385).read_to_end(&mut b).unwrap();
            b
        });
        let mut input = child.stdin.take().unwrap();
        let data = data.to_vec();
        let writer = std::thread::spawn(move || input.write_all(&data));
        let end = Instant::now() + Duration::from_secs(650);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= end {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("bootstrap timeout");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let _ = writer.join().unwrap(); // Early rejection may close stdin.
        let output = stdout.join().unwrap();
        let error = stderr.join().unwrap();
        assert_eq!(
            status.code(),
            Some(exit),
            "bootstrap exit; stderr={}",
            String::from_utf8_lossy(&error)
        );
        assert!(output.len() <= 16384 && error.len() <= 16384);
        output
    }
    fn probe() -> Probe {
        let nonce = "12345678123456781234567812345678";
        Probe::parse(
            &execute(
                bootstrap::windows_probe_command(),
                format!("{nonce}\n").as_bytes(),
                0,
            ),
            nonce,
        )
        .unwrap()
    }
    let before = probe();
    assert!(
        before.elevated && before.agent_version.is_none(),
        "requires a fresh VM"
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let config = format!("{{\"transport\":\"direct\",\"port\":{port}}}");
    let b64 = base64::engine::general_purpose::STANDARD;
    let encoded = b64.encode(config.as_bytes());
    let bundle = b64.encode(binary);
    execute(
        bootstrap::windows_install_command(),
        format!("UMBOOT1\n{}\n{bundle}\n{encoded}\n", "0".repeat(64)).as_bytes(),
        74,
    );
    assert!(
        probe().agent_version.is_none(),
        "bad digest must not install"
    );
    execute(
        bootstrap::windows_install_command(),
        format!("UMBOOT1\n{hash}\n{bundle}\n{encoded}\nextra\n").as_bytes(),
        70,
    );
    assert!(probe().agent_version.is_none());
    execute(
        bootstrap::windows_install_command(),
        format!("UMBOOT1\n{hash}\n{bundle}\n{encoded}\n").as_bytes(),
        0,
    );
    let after = probe();
    assert!(after.usb_stack && after.service_running);
    // The connection library and desktop application have independent versions.
    assert_eq!(after.agent_version.as_deref(), Some(version));
    assert_eq!(after.agent_sha256.as_deref(), Some(hash.as_str()));
    execute(
        bootstrap::windows_configure_command(),
        format!("UMCONF1\n{}\n{encoded}\n", "0".repeat(64)).as_bytes(),
        78,
    );
    execute(
        bootstrap::windows_configure_command(),
        format!("UMCONF1\n{hash}\n{encoded}\nextra\n").as_bytes(),
        70,
    );
    execute(
        bootstrap::windows_configure_command(),
        format!("UMCONF1\n{hash}\n{}\n", b64.encode(b"not-json")).as_bytes(),
        79,
    );
    let payload = format!("UMCONF1\n{hash}\n{encoded}\n");
    assert!(payload.len() < 1024);
    execute(
        bootstrap::windows_configure_command(),
        payload.as_bytes(),
        0,
    );
    assert_eq!(probe(), after);
    println!(
        "PASS Windows bootstrap: real cmd.exe/PowerShell, bounded command, bad hash/extra stdin rejection, cold signed stack + SCM, verified probe, config-only reuse and invalid config rejection"
    );
}
