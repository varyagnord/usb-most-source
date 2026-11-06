//! Test-only authenticated loopback fan-out to HID, RAM MSC and synthetic CCID.
//! No host device discovery, filesystem mounts, USB handles or cryptography.
#[path = "../../../tests/fixtures/usb_ccid.rs"]
mod ccid;
#[path = "receiver_lab/usb.rs"]
mod hid;
#[path = "../../../tests/fixtures/usb_storage.rs"]
mod storage;

use std::{
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Device {
    address: SocketAddr,
    descriptor: Vec<u8>,
    interfaces: Vec<u8>,
    busy: AtomicBool,
}
impl Device {
    fn open(address: SocketAddr) -> Self {
        assert!(address.ip().is_loopback());
        let mut records = usb_most_protocol::agent::devices(address).unwrap();
        assert_eq!(records.len(), 1);
        let (descriptor, interfaces) = records.pop().unwrap();
        assert_eq!(descriptor.len(), 312);
        assert_eq!(interfaces.len(), usize::from(descriptor[311]) * 4);
        Self {
            address,
            descriptor,
            interfaces,
            busy: AtomicBool::new(false),
        }
    }
}
struct Lease(Arc<Device>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}
async fn reply(stream: &mut TcpStream, op: u16, status: u32) -> io::Result<()> {
    stream.write_all(&[1, 17]).await?;
    stream.write_all(&op.to_be_bytes()).await?;
    stream.write_all(&status.to_be_bytes()).await
}
async fn serve(
    mut stream: TcpStream,
    proof: [u8; 32],
    devices: Arc<Vec<Arc<Device>>>,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut auth = [0; 40];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut auth)).await??;
    if &auth[..8] != b"UMLOCAL1" || auth[8..] != proof {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    stream.write_all(&[1]).await?;
    let mut header = [0; 8];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut header)).await??;
    if header[..2] != [1, 17] || header[4..] != [0; 4] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    match u16::from_be_bytes(header[2..4].try_into().unwrap()) {
        0x8005 => {
            reply(&mut stream, 5, 0).await?;
            stream
                .write_all(&(devices.len() as u32).to_be_bytes())
                .await?;
            for device in devices.iter() {
                stream.write_all(&device.descriptor).await?;
                stream.write_all(&device.interfaces).await?;
            }
            Ok(())
        }
        0x8003 => {
            let mut busid = [0; 32];
            tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut busid)).await??;
            let Some(device) = devices.iter().find(|d| d.descriptor[256..288] == busid) else {
                return reply(&mut stream, 3, 1).await;
            };
            if device
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return reply(&mut stream, 3, 1).await;
            }
            let _lease = Lease(device.clone());
            let mut backend = TcpStream::connect(device.address).await?;
            backend.set_nodelay(true)?;
            backend.write_all(&header).await?;
            backend.write_all(&busid).await?;
            let mut descriptor = [0; 320];
            tokio::time::timeout(Duration::from_secs(5), backend.read_exact(&mut descriptor))
                .await??;
            if descriptor[..8] != [1, 17, 0, 3, 0, 0, 0, 0] || descriptor[8..] != device.descriptor
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
            stream.write_all(&descriptor).await?;
            tokio::io::copy_bidirectional(&mut stream, &mut backend).await?;
            Ok(())
        }
        _ => reply(&mut stream, 0xdf, 95).await,
    }
}
fn main() {
    use base64::Engine;
    assert_eq!(
        std::env::var("USB_MOST_OWNED_EMULATOR_LAB").as_deref(),
        Ok("1")
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let output = PathBuf::from(std::env::args().nth(1).unwrap())
        .canonicalize()
        .unwrap();
    assert!(output.starts_with(root.join("build/e2e")));
    let mouse = hid::Fixture::start(false);
    let disk = storage::Fixture::start();
    disk.counts.trace.store(true, Ordering::Relaxed);
    let card = ccid::Fixture::start();
    let devices = Arc::new(vec![
        Arc::new(Device::open(mouse.address)),
        Arc::new(Device::open(disk.address)),
        Arc::new(Device::open(card.address)),
    ]);
    let proof = mouse.proof;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(output.join("bridge.json")).unwrap();
        use std::io::Write;
        file.write_all(serde_json::to_string(&serde_json::json!({"port":listener.local_addr().unwrap().port(),"proof":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(proof),"devices":3,"scope":"RAM-only mixed synthetic USB"})).unwrap().as_bytes()).unwrap();file.sync_all().unwrap();
        let mut jobs=tokio::task::JoinSet::new();let mut tick=tokio::time::interval(Duration::from_millis(100));
        let end=tokio::time::Instant::now()+Duration::from_secs(1600);
        let (mut armed, mut released) = (false, false);
        loop {
            tokio::select! {
                _=tick.tick()=>{
                    if output.join("stop-fixture").exists() || tokio::time::Instant::now()>=end {break;}
                    if !armed && output.join("arm-write-fence").exists() {
                        disk.counts.write_fence.arm().unwrap(); armed=true;
                    }
                    if !released && output.join("release-write-fence").exists() {
                        disk.counts.write_fence.release().unwrap(); released=true;
                    }
                    if armed {
                        let state=disk.counts.write_fence.snapshot();
                        let snapshot=serde_json::json!({"phase":format!("{:?}",state.phase),
                            "commits":state.commits,"bytes":state.bytes,"first_import":state.first_import,
                            "first_tag":state.first_tag,"total_bytes_written":disk.counts.bytes_written.load(Ordering::Relaxed)});
                        std::fs::write(output.join("write-fence.part"),serde_json::to_vec(&snapshot).unwrap()).unwrap();
                        std::fs::rename(output.join("write-fence.part"),output.join("write-fence.json")).unwrap();
                    }
                },
                Some(result)=jobs.join_next(),if !jobs.is_empty()=>{result.unwrap();},
                incoming=listener.accept(),if jobs.len()<32=>{
                    let (stream,_)=incoming.unwrap();let devices=devices.clone();
                    jobs.spawn(async move {if let Err(error)=serve(stream,proof,devices).await {eprintln!("Mixed fixture connection ended: {:?}",error.kind());}});
                },
            }
        }
        jobs.abort_all();while jobs.join_next().await.is_some() {}
    });
    let fence=disk.counts.write_fence.snapshot();
    std::fs::write(output.join("usb-result.json"),serde_json::to_vec(&serde_json::json!({
        "write_fence":{"phase":format!("{:?}",fence.phase),"commits":fence.commits,"bytes":fence.bytes,"first_import":fence.first_import,"first_tag":fence.first_tag},
        "hid":mouse.stats(),"storage":{"imports":disk.counts.imports.load(Ordering::Relaxed),"commands":disk.counts.commands.load(Ordering::Relaxed),"reads":disk.counts.reads.load(Ordering::Relaxed),"writes":disk.counts.writes.load(Ordering::Relaxed),"syncs":disk.counts.syncs.load(Ordering::Relaxed),"bytes_written":disk.counts.bytes_written.load(Ordering::Relaxed)},
        "ccid":{"imports":card.counts.imports.load(Ordering::Relaxed),"commands":card.counts.commands.load(Ordering::Relaxed),"exchanges":card.counts.exchanges.load(Ordering::Relaxed)}
    })).unwrap()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn client(
        listener: &TcpListener,
        proof: [u8; 32],
        devices: Arc<Vec<Arc<Device>>>,
        jobs: &mut Vec<tokio::task::JoinHandle<io::Result<()>>>,
    ) -> TcpStream {
        let mut c = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        jobs.push(tokio::spawn(serve(server, proof, devices)));
        c.write_all(b"UMLOCAL1").await.unwrap();
        c.write_all(&proof).await.unwrap();
        assert_eq!(c.read_u8().await.unwrap(), 1);
        c
    }
    async fn import(c: &mut TcpStream, id: &str) -> u32 {
        c.write_all(&[1, 17, 0x80, 3, 0, 0, 0, 0]).await.unwrap();
        let mut busid = [0; 32];
        busid[..id.len()].copy_from_slice(id.as_bytes());
        c.write_all(&busid).await.unwrap();
        let mut header = [0; 8];
        c.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..4], &[1, 17, 0, 3]);
        u32::from_be_bytes(header[4..].try_into().unwrap())
    }
    #[test]
    fn independent_catalog_and_import_leases_over_real_sockets() {
        let mouse = hid::Fixture::start(false);
        let disk = storage::Fixture::start();
        let card = ccid::Fixture::start();
        let devices = Arc::new(vec![
            Arc::new(Device::open(mouse.address)),
            Arc::new(Device::open(disk.address)),
            Arc::new(Device::open(card.address)),
        ]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut jobs = vec![];
            let mut c = client(&listener, [7; 32], devices.clone(), &mut jobs).await;
            c.write_all(&[1, 17, 0x80, 5, 0, 0, 0, 0]).await.unwrap();
            let mut catalog = vec![0; 960];
            c.read_exact(&mut catalog).await.unwrap();
            assert_eq!(u32::from_be_bytes(catalog[8..12].try_into().unwrap()), 3);
            for (at, id) in [(12, "1-7"), (328, "1-8"), (644, "1-9")] {
                assert_eq!(&catalog[at + 256..at + 259], id.as_bytes());
            }
            let mut unknown = client(&listener, [7; 32], devices.clone(), &mut jobs).await;
            assert_eq!(import(&mut unknown, "1-99").await, 1);
            let mut first = client(&listener, [7; 32], devices.clone(), &mut jobs).await;
            assert_eq!(import(&mut first, "1-9").await, 0);
            let mut descriptor = [0; 312];
            first.read_exact(&mut descriptor).await.unwrap();
            let mut duplicate = client(&listener, [7; 32], devices.clone(), &mut jobs).await;
            assert_eq!(import(&mut duplicate, "1-9").await, 1);
            let mut independent = client(&listener, [7; 32], devices.clone(), &mut jobs).await;
            assert_eq!(import(&mut independent, "1-8").await, 0);
            independent.read_exact(&mut descriptor).await.unwrap();
            assert_eq!(card.counts.imports.load(Ordering::Relaxed), 1);
            assert_eq!(disk.counts.imports.load(Ordering::Relaxed), 1);
            let mut bad = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (server, _) = listener.accept().await.unwrap();
            let rejected = tokio::spawn(serve(server, [7; 32], devices.clone()));
            bad.write_all(&[0; 40]).await.unwrap();
            assert_eq!(
                rejected.await.unwrap().unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            for job in jobs {
                job.abort();
                let _ = job.await;
            }
            assert!(
                devices
                    .iter()
                    .all(|device| !device.busy.load(Ordering::Acquire))
            );
        });
    }
}
