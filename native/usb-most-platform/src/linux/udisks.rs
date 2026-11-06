use super::{
    eject::{self, Block, Drive, Host, Snapshot},
    owned_usb::{OwnedUsb, PinnedPath},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::Duration,
};
use usb_most_protocol::eject::Outcome;
use zbus::{Connection, DBusError, ObjectPath, OwnedVariant, Variant, fdo::ManagedObjects};

const SERVICE: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";

pub(super) async fn eject(device: &OwnedUsb) -> Outcome {
    if let Err(outcome) = device.storage_only() {
        return outcome;
    }
    let setup = async {
        // Deliberately ignore DBUS_SYSTEM_BUS_ADDRESS inherited by a service.
        // Never use a session bus or a caller-supplied address for removal.
        let connection =
            zbus::connection::Builder::address("unix:path=/run/dbus/system_bus_socket")
                .max_queued(std::num::NonZeroUsize::new(8).unwrap())
                .build()
                .await?;
        let owner = service_owner(&connection).await?;
        Ok::<_, zbus::Error>((connection, owner))
    };
    let (connection, owner) = match tokio::time::timeout(Duration::from_secs(5), setup).await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            return match classify(&error, false) {
                Outcome::Unavailable => Outcome::Unsupported,
                outcome => outcome,
            };
        }
        Err(_) => return Outcome::Unsupported,
    };
    let mut host = UDisks {
        connection,
        owner,
        device,
        pins: BTreeMap::new(),
        expected: None,
    };
    eject::execute(&mut host).await
}

// GetNameOwner alone does not activate a service installed for on-demand use.
// Ask the system bus to start only its administrator-configured UDisks service;
// masked/missing/denied services remain an error. Pin the resulting unique
// owner before inspecting devices; a restart never redirects an old operation.
async fn service_owner(
    connection: &Connection,
) -> Result<zbus::names::OwnedUniqueName, zbus::Error> {
    let bus = zbus::fdo::DBusProxy::new(connection).await?;
    match bus.get_name_owner(SERVICE.try_into()?).await {
        Ok(owner) => Ok(owner),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
            bus.start_service_by_name(SERVICE.try_into()?, 0).await?;
            Ok(bus.get_name_owner(SERVICE.try_into()?).await?)
        }
        Err(error) => Err(error.into()),
    }
}

struct UDisks<'a> {
    connection: Connection,
    owner: zbus::names::OwnedUniqueName,
    device: &'a OwnedUsb,
    pins: BTreeMap<u64, PinnedPath>,
    expected: Option<BTreeSet<u64>>,
}

impl UDisks<'_> {
    /// Kernel enumeration and a usable filesystem can precede UDisks' probe.
    /// Wait only for initial, read-only discovery of this exact still-owned
    /// import. Never retry a mutation, replace the pinned bus owner, or wait for
    /// a replacement after a plan was already selected.
    async fn wait_for_owned_snapshot(&mut self, budget: Duration) -> Result<Snapshot, Outcome> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut delay = Duration::from_millis(50);
        loop {
            // Only a read-only metadata query is cancelled at this deadline.
            // Mutating UDisks calls retain their original lifetime contract.
            let (snapshot, complete) =
                match tokio::time::timeout_at(deadline, self.read_snapshot()).await {
                    Ok(snapshot) => snapshot?,
                    Err(_) => {
                        self.current()?;
                        return Err(Outcome::Unsupported);
                    }
                };
            if complete {
                self.expected = Some(
                    snapshot
                        .blocks
                        .iter()
                        .filter(|b| b.owned)
                        .map(|b| b.number)
                        .collect(),
                );
                return Ok(snapshot);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                self.current()?;
                // The kernel owner still exists, but this OS management
                // service cannot operate on it. Do not call this USB loss.
                return Err(Outcome::Unsupported);
            }
            tokio::time::sleep(delay.min(remaining)).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
    }
    fn current(&self) -> Result<(), Outcome> {
        self.device.storage_only()?;
        for pin in self.pins.values() {
            pin.check().map_err(|_| Outcome::Unavailable)?;
        }
        Ok(())
    }
    fn pin_block(&mut self, number: u64, pin: PinnedPath) -> Result<(), Outcome> {
        if let Some(old) = self.pins.get(&number) {
            old.check().map_err(|_| Outcome::Unavailable)?;
            if old.path != pin.path {
                return Err(Outcome::Unavailable);
            }
        } else {
            if self.pins.len() >= 128 {
                return Err(Outcome::Unsupported);
            }
            self.pins.insert(number, pin);
        }
        Ok(())
    }
    fn inventory(&mut self) -> Result<BTreeSet<u64>, Outcome> {
        let blocks = self.device.blocks()?;
        let numbers = blocks.keys().copied().collect();
        for (number, pin) in blocks {
            self.pin_block(number, pin)?;
        }
        self.current()?;
        Ok(numbers)
    }
    async fn mutate(&self, path: &str, interface: &str, method: &str) -> Result<(), Outcome> {
        self.current()?;
        // Close the gap between the final metadata query and the OS call.
        // Newly discovered partitions must never be silently omitted. Pinned
        // paths still reject same-number replacement, including before D-Bus
        // first discovered the child.
        let current = self.device.blocks()?.into_keys().collect::<BTreeSet<_>>();
        if self.expected.as_ref() != Some(&current) {
            return Err(Outcome::Unavailable);
        }
        self.current()?;
        let options = HashMap::from([("auth.no_user_interaction", Variant::from(true))]);
        // Unique bus owner prevents a restarted service accepting an old path.
        // No method timeout here: the coordinator may report a UI timeout while
        // retaining this future/import. A bus failure is an uncertain outcome.
        let reply = self
            .connection
            .call_method(
                Some(self.owner.as_str()),
                path,
                Some(interface),
                method,
                &(options,),
            )
            .await;
        match reply {
            Ok(message) => message
                .body()
                .deserialize::<()>()
                .map_err(|_| Outcome::Uncertain),
            Err(error)
                if method == "Unmount"
                    && error_name(&error) == Some("org.freedesktop.UDisks2.Error.NotMounted") =>
            {
                Ok(())
            }
            Err(error) => Err(classify(&error, true)),
        }
    }
    async fn read_snapshot(&mut self) -> Result<(Snapshot, bool), Outcome> {
        self.current()?;
        let before = self.inventory()?;
        let call = self.connection.call_method(
            Some(self.owner.as_str()),
            ROOT,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        );
        let message = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .map_err(|_| Outcome::Unavailable)?
            .map_err(|e| classify(&e, false))?;
        // Bound deserialization of local service metadata; do not log its raw
        // values, volume labels, mount paths, serials or D-Bus error descriptions.
        if message.body().data().len() > 8 * 1024 * 1024 {
            return Err(Outcome::Unsupported);
        }
        let objects: ManagedObjects = message
            .body()
            .deserialize()
            .map_err(|_| Outcome::Unsupported)?;
        let (result, metadata_ready) = parse(objects, |number| {
            let Some(pin) = self.device.block(number)? else {
                return Ok(false);
            };
            self.pin_block(number, pin)?;
            Ok(true)
        })?;
        let after = self.inventory()?;
        let observed = result
            .blocks
            .iter()
            .filter(|b| b.owned)
            .map(|b| b.number)
            .collect::<BTreeSet<_>>();
        let complete = metadata_ready && !after.is_empty() && before == after && observed == after;
        Ok((result, complete))
    }
}

impl Host for UDisks<'_> {
    async fn initial_snapshot(&mut self) -> Result<Snapshot, Outcome> {
        // The Android deadline may show Uncertain earlier. The coordinator
        // retains this operation and accepts its eventual exact-key result;
        // no additional user request or replay is created by waiting here.
        self.wait_for_owned_snapshot(Duration::from_secs(120)).await
    }
    async fn snapshot(&mut self) -> Result<Snapshot, Outcome> {
        let (snapshot, complete) = self.read_snapshot().await?;
        if !complete {
            return Err(Outcome::Unavailable);
        }
        Ok(snapshot)
    }
    async fn unmount(&mut self, block: &str) -> Result<(), Outcome> {
        self.mutate(block, FILESYSTEM, "Unmount").await
    }
    async fn power_off(&mut self, drive: &str) -> Result<(), Outcome> {
        self.mutate(drive, DRIVE, "PowerOff").await
    }
}

type Props = HashMap<String, OwnedVariant>;
fn value<'a>(props: &'a Props, key: &str) -> Result<&'a OwnedVariant, Outcome> {
    props.get(key).ok_or(Outcome::Unsupported)
}
fn string(props: &Props, key: &str) -> Result<String, Outcome> {
    let s = <&str>::try_from(value(props, key)?).map_err(|_| Outcome::Unsupported)?;
    if s.len() > 4096 {
        return Err(Outcome::Unsupported);
    }
    Ok(s.to_owned())
}
fn object(props: &Props, key: &str) -> Result<String, Outcome> {
    let path = <&ObjectPath<'_>>::try_from(value(props, key)?).map_err(|_| Outcome::Unsupported)?;
    if path.as_str().len() > 4096 {
        return Err(Outcome::Unsupported);
    }
    Ok(path.as_str().to_owned())
}
fn uint(props: &Props, key: &str) -> Result<u64, Outcome> {
    u64::try_from(value(props, key)?).map_err(|_| Outcome::Unsupported)
}
fn boolean(props: &Props, key: &str) -> Result<bool, Outcome> {
    bool::try_from(value(props, key)?).map_err(|_| Outcome::Unsupported)
}

fn parse(
    objects: ManagedObjects,
    mut owned: impl FnMut(u64) -> Result<bool, Outcome>,
) -> Result<(Snapshot, bool), Outcome> {
    if objects.len() > 4096 {
        return Err(Outcome::Unsupported);
    }
    let mut snapshot = Snapshot::default();
    let mut metadata_ready = true;
    for (path, interfaces) in objects {
        if path.as_str().len() > 4096
            || !path.as_str().starts_with(&format!("{ROOT}/"))
            || interfaces.len() > 32
            || interfaces.values().any(|p| p.len() > 128)
        {
            return Err(Outcome::Unsupported);
        }
        if let Some(props) = interfaces.get(DRIVE) {
            snapshot.drives.push(Drive {
                path: path.as_str().to_owned(),
                usb: string(props, "ConnectionBus")? == "usb",
                can_power_off: boolean(props, "CanPowerOff")?,
                sibling: string(props, "SiblingId")?,
                detected: uint(props, "TimeDetected")?,
            });
        }
        if let Some(props) = interfaces.get(BLOCK) {
            let number = uint(props, "DeviceNumber")?;
            let is_owned = owned(number)?;
            let drive = object(props, "Drive")?;
            let filesystem = interfaces.get(FILESYSTEM);
            let mounted = if is_owned {
                if let Some(filesystem) = filesystem {
                    let points = <&zbus::Array<'_>>::try_from(value(filesystem, "MountPoints")?)
                        .map_err(|_| Outcome::Unsupported)?;
                    if points.len() > 128 || points.element_signature().to_string() != "ay" {
                        return Err(Outcome::Unsupported);
                    }
                    !points.is_empty()
                } else {
                    false
                }
            } else {
                false
            };
            let usage = string(props, "IdUsage")?;
            if is_owned && usage == "filesystem" && filesystem.is_none() {
                metadata_ready = false;
            }
            let usable = is_owned
                && matches!(usage.as_str(), "" | "filesystem")
                && object(props, "MDRaid")? == "/"
                && object(props, "MDRaidMember")? == "/"
                && object(props, "CryptoBackingDevice")? == "/";
            snapshot.blocks.push(Block {
                path: path.as_str().to_owned(),
                number,
                drive,
                owned: is_owned,
                usable,
                filesystem: filesystem.is_some(),
                mounted,
            });
        }
    }
    metadata_ready &= snapshot
        .blocks
        .iter()
        .filter(|b| b.owned)
        .all(|b| snapshot.drives.iter().any(|d| d.path == b.drive));
    Ok((snapshot, metadata_ready))
}

fn error_name(error: &zbus::Error) -> Option<&str> {
    match error {
        zbus::Error::MethodError(name, _, _) => Some(name.as_str()),
        _ => None,
    }
}
fn classify(error: &zbus::Error, mutation: bool) -> Outcome {
    let name = match error {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_owned(),
        zbus::Error::FDO(e) => e.name().as_str().to_owned(),
        _ => String::new(),
    };
    match name.as_str() {
        "org.freedesktop.UDisks2.Error.DeviceBusy" => Outcome::Busy,
        "org.freedesktop.UDisks2.Error.NotAuthorized"
        | "org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain"
        | "org.freedesktop.UDisks2.Error.NotAuthorizedDismissed"
        | "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.AuthFailed" => Outcome::Denied,
        "org.freedesktop.UDisks2.Error.NotSupported"
        | "org.freedesktop.DBus.Error.UnknownMethod"
        | "org.freedesktop.DBus.Error.UnknownInterface" => Outcome::Unsupported,
        _ if mutation => Outcome::Uncertain,
        _ => Outcome::Unavailable,
    }
}

#[cfg(test)]
mod tests;
