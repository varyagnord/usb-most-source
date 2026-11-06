//! Selection and sequencing without D-Bus or filesystem side effects.
use usb_most_protocol::eject::Outcome;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Block {
    pub path: String,
    pub number: u64,
    pub drive: String,
    pub owned: bool,
    pub usable: bool,
    pub filesystem: bool,
    pub mounted: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Drive {
    pub path: String,
    pub usb: bool,
    pub can_power_off: bool,
    pub sibling: String,
    pub detected: u64,
}
#[derive(Default)]
pub(super) struct Snapshot {
    pub blocks: Vec<Block>,
    pub drives: Vec<Drive>,
}
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    drive: Drive,
    blocks: Vec<Block>,
}
impl Plan {
    fn select(snapshot: Snapshot) -> Result<Self, Outcome> {
        if snapshot.blocks.len() > 4096 || snapshot.drives.len() > 4096 {
            return Err(Outcome::Unsupported);
        }
        let first = snapshot
            .blocks
            .iter()
            .find(|b| b.owned)
            .ok_or(Outcome::Unavailable)?;
        let drive = snapshot
            .drives
            .iter()
            .find(|d| d.path == first.drive)
            .ok_or(Outcome::Unsupported)?;
        if !drive.usb
            || !drive.can_power_off
            || drive.detected == 0
            || snapshot
                .drives
                .iter()
                .filter(|d| d.path == drive.path)
                .count()
                != 1
            || snapshot.drives.iter().any(|d| {
                !drive.sibling.is_empty() && d.path != drive.path && d.sibling == drive.sibling
            })
            || snapshot
                .blocks
                .iter()
                .any(|b| (b.owned && b.drive != drive.path) || (b.drive == drive.path && !b.owned))
        {
            return Err(Outcome::Unsupported);
        }
        let mut blocks: Vec<_> = snapshot.blocks.into_iter().filter(|b| b.owned).collect();
        if blocks.len() > 128
            || blocks
                .iter()
                .any(|b| !b.usable || (b.mounted && !b.filesystem))
        {
            return Err(Outcome::Unsupported);
        }
        blocks.sort_by(|a, b| a.path.cmp(&b.path));
        let mut numbers = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        if blocks
            .iter()
            .any(|b| !numbers.insert(b.number) || !paths.insert(&b.path))
        {
            return Err(Outcome::Unsupported);
        }
        Ok(Self {
            drive: drive.clone(),
            blocks,
        })
    }
    fn same_identity(&self, other: &Self) -> bool {
        self.drive == other.drive
            && self.blocks.len() == other.blocks.len()
            && self.blocks.iter().zip(&other.blocks).all(|(a, b)| {
                a.path == b.path
                    && a.number == b.number
                    && a.drive == b.drive
                    && a.owned == b.owned
                    && a.usable == b.usable
                    && a.filesystem == b.filesystem
            })
    }
}

pub(super) trait Host {
    // Only initial discovery may wait for the OS inventory to catch up. Once
    // a plan is pinned, missing or changed objects are an ownership failure.
    async fn initial_snapshot(&mut self) -> Result<Snapshot, Outcome> {
        self.snapshot().await
    }
    // Must check the exact USB owner and pinned block paths on EVERY snapshot
    // and immediately before a mutation. Never use cached D-Bus properties.
    async fn snapshot(&mut self) -> Result<Snapshot, Outcome>;
    async fn unmount(&mut self, block: &str) -> Result<(), Outcome>;
    async fn power_off(&mut self, drive: &str) -> Result<(), Outcome>;
}

async fn run(host: &mut impl Host) -> Result<(), Outcome> {
    let plan = Plan::select(host.initial_snapshot().await?)?;
    for block in &plan.blocks {
        let now = Plan::select(host.snapshot().await?)?;
        if !plan.same_identity(&now) {
            return Err(Outcome::Unavailable);
        }
        if now.blocks.iter().any(|b| b.path == block.path && b.mounted) {
            host.unmount(&block.path).await?;
        }
    }
    let now = Plan::select(host.snapshot().await?)?;
    if !plan.same_identity(&now) {
        return Err(Outcome::Unavailable);
    }
    if now.blocks.iter().any(|b| b.mounted) {
        return Err(Outcome::Busy);
    }
    host.power_off(&plan.drive.path).await
}

pub(super) async fn execute(host: &mut impl Host) -> Outcome {
    match run(host).await {
        Ok(()) => Outcome::Safe,
        Err(outcome) => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn disk() -> Snapshot {
        Snapshot {
            drives: vec![Drive {
                path: "/drive/one".into(),
                usb: true,
                can_power_off: true,
                sibling: "usb3/3-1".into(),
                detected: 42,
            }],
            blocks: (0..3)
                .map(|i| Block {
                    path: format!("/block/disk{i}"),
                    number: 2048 + i,
                    drive: "/drive/one".into(),
                    owned: true,
                    usable: true,
                    filesystem: i != 0,
                    mounted: i != 0,
                })
                .collect(),
        }
    }
    #[test]
    fn foreign_drive_shared_lun_and_incomplete_ownership_are_rejected() {
        for kind in 0..11 {
            let mut s = disk();
            match kind {
                0 => s.drives[0].usb = false,
                1 => s.drives[0].can_power_off = false,
                2 => s.drives[0].detected = 0,
                3 => s.blocks[0].drive = "/drive/other".into(),
                4 => s.blocks[0].owned = false,
                5 => s.blocks[0].usable = false,
                6 => s.blocks[0].mounted = true,
                7 => {
                    let mut d = s.drives[0].clone();
                    d.path = "/drive/foreign".into();
                    s.drives.push(d);
                }
                8 => s.blocks.push(s.blocks[0].clone()),
                9 => s.drives.push(s.drives[0].clone()),
                10 => s.blocks[1].number = s.blocks[0].number,
                _ => unreachable!(),
            }
            assert_eq!(
                Plan::select(s).unwrap_err(),
                Outcome::Unsupported,
                "case {kind}"
            );
        }
    }
    struct Fake {
        calls: Vec<String>,
        mounted: [bool; 3],
        fail: Option<Outcome>,
        fail_at: usize,
        replace_at: usize,
        snapshots: usize,
        remount: bool,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                calls: vec![],
                mounted: [false, true, true],
                fail: None,
                fail_at: 1,
                replace_at: usize::MAX,
                snapshots: 0,
                remount: false,
            }
        }
    }
    impl Host for Fake {
        async fn snapshot(&mut self) -> Result<Snapshot, Outcome> {
            self.snapshots += 1;
            let mut s = disk();
            if self.snapshots >= self.replace_at {
                s.drives[0].detected += 1;
            }
            for (b, mounted) in s.blocks.iter_mut().zip(self.mounted) {
                b.mounted = mounted;
            }
            if self.remount && self.calls.len() >= 2 {
                s.blocks[1].mounted = true;
            }
            Ok(s)
        }
        async fn unmount(&mut self, path: &str) -> Result<(), Outcome> {
            self.calls.push(format!("unmount:{path}"));
            if let Some(error) = self.fail
                && self.calls.len() == self.fail_at
            {
                return Err(error);
            }
            self.mounted[path.as_bytes().last().unwrap().wrapping_sub(b'0') as usize] = false;
            Ok(())
        }
        async fn power_off(&mut self, path: &str) -> Result<(), Outcome> {
            self.calls.push(format!("poweroff:{path}"));
            Ok(())
        }
    }
    fn run_test(future: impl std::future::Future<Output = ()>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future);
    }
    #[test]
    fn unmounts_all_volumes_and_only_then_requests_power_off() {
        run_test(async {
            let mut host = Fake::new();
            assert_eq!(execute(&mut host).await, Outcome::Safe);
            assert_eq!(
                host.calls,
                [
                    "unmount:/block/disk1",
                    "unmount:/block/disk2",
                    "poweroff:/drive/one"
                ]
            );
        });
    }
    #[test]
    fn busy_denied_unknown_and_disappeared_never_trigger_power_off() {
        run_test(async {
            for error in [
                Outcome::Busy,
                Outcome::Denied,
                Outcome::Uncertain,
                Outcome::Unavailable,
            ] {
                let mut host = Fake::new();
                host.fail = Some(error);
                assert_eq!(execute(&mut host).await, error);
                assert_eq!(host.calls.len(), 1);
            }
        });
    }
    #[test]
    fn partial_unmount_stays_unmounted_and_explicit_retry_finishes_remaining_volume() {
        run_test(async {
            let mut host = Fake::new();
            host.fail = Some(Outcome::Busy);
            host.fail_at = 2;
            assert_eq!(execute(&mut host).await, Outcome::Busy);
            assert_eq!(host.mounted, [false, false, true]);
            assert_eq!(host.calls, ["unmount:/block/disk1", "unmount:/block/disk2"]);
            host.fail = None;
            // This second call represents a new explicit, ledger-authorized
            // request. No automatic retry is performed by execute itself.
            assert_eq!(execute(&mut host).await, Outcome::Safe);
            assert_eq!(
                host.calls,
                [
                    "unmount:/block/disk1",
                    "unmount:/block/disk2",
                    "unmount:/block/disk2",
                    "poweroff:/drive/one"
                ]
            );
        });
    }
    #[test]
    fn replacement_or_remount_stops_before_next_side_effect() {
        run_test(async {
            for at in [2, 3, 4, 5] {
                let mut host = Fake::new();
                host.replace_at = at;
                assert_eq!(execute(&mut host).await, Outcome::Unavailable);
                assert!(!host.calls.iter().any(|s| s.starts_with("poweroff")));
            }
            let mut host = Fake::new();
            host.remount = true;
            assert_eq!(execute(&mut host).await, Outcome::Busy);
            assert_eq!(host.calls.len(), 2);
        });
    }
}
