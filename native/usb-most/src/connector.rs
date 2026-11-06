//! Auto-imports only devices explicitly exported by Android selection/permission policy.
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    process::Command,
    time::Duration,
};
#[derive(Default)]
pub struct Retry {
    failures: BTreeMap<String, (u32, u64)>,
}
impl Retry {
    pub fn candidates(
        &mut self,
        exports: &[String],
        attached: &BTreeSet<String>,
        now: u64,
    ) -> Vec<String> {
        self.failures.retain(|k, _| exports.contains(k));
        exports
            .iter()
            .filter(|id| {
                valid(id)
                    && !attached.contains(*id)
                    && self.failures.get(*id).is_none_or(|(_, at)| now >= *at)
            })
            .cloned()
            .collect()
    }
    pub fn attempted(&mut self, id: &str, success: bool, now: u64) {
        if success {
            self.failures.remove(id);
        } else {
            let n = self.failures.get(id).map_or(0, |n| n.0);
            let backoff = [5, 15, 30, 60, 120, 300][(n as usize).min(5)];
            self.failures
                .insert(id.to_owned(), (n.saturating_add(1), now + backoff));
        }
    }
}
fn valid(s: &str) -> bool {
    let mut parts = s.split('-');
    let a = parts.next().unwrap_or("");
    let b = parts.next().unwrap_or("");
    parts.next().is_none()
        && [a, b].iter().all(|p| {
            !p.is_empty()
                && p.len() <= 6
                && !p.starts_with('0')
                && p.bytes().all(|c| c.is_ascii_digit())
        })
}
pub fn run(local: SocketAddr) {
    let start = std::time::Instant::now();
    let mut retry = Retry::default();
    loop {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let devices = crate::agent::devices(local)?;
            let mut exports = Vec::new();
            for (d, _) in devices {
                let b = &d[256..288];
                let end = b.iter().position(|c| *c == 0).ok_or("busid")?;
                let s = std::str::from_utf8(&b[..end])?;
                if valid(s) {
                    exports.push(s.to_owned());
                }
            }
            let output = Command::new("/usr/bin/timeout")
                .args(["--kill-after=1s", "8s", "/usr/sbin/usbip", "port"])
                .output()?;
            if !output.status.success() {
                return Err("usbip port".into());
            }
            let ports = String::from_utf8(output.stdout)?;
            let marker = format!(" -> usbip://{local}/");
            let attached = ports
                .lines()
                .filter_map(|line| line.split_once(&marker).map(|(_, id)| id.trim().to_owned()))
                .collect::<BTreeSet<_>>();
            for id in retry.candidates(&exports, &attached, start.elapsed().as_secs()) {
                let success = Command::new("/usr/bin/timeout")
                    .args([
                        "--kill-after=1s",
                        "90s",
                        "/usr/sbin/usbip",
                        "--tcp-port",
                        &local.port().to_string(),
                        "attach",
                        "-r",
                        &local.ip().to_string(),
                        "-b",
                        &id,
                    ])
                    .output()?
                    .status
                    .success();
                retry.attempted(&id, success, start.elapsed().as_secs());
            }
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("USB Most auto-connect: {e}");
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exported_valid_devices_and_backoff() {
        let mut r = Retry::default();
        let d = vec!["1-6".into(), "1-5".into(), ";reboot".into(), "01-3".into()];
        let attached = BTreeSet::from(["1-5".into()]);
        assert_eq!(r.candidates(&d, &attached, 0), ["1-6"]);
        r.attempted("1-6", false, 0);
        assert!(r.candidates(&d, &attached, 4).is_empty());
        assert_eq!(r.candidates(&d, &attached, 5), ["1-6"]);
        r.attempted("1-6", true, 5);
        assert_eq!(r.candidates(&d, &attached, 5), ["1-6"]);
        assert!(r.candidates(&[], &attached, 6).is_empty());
        assert!(r.failures.is_empty());
    }
}
