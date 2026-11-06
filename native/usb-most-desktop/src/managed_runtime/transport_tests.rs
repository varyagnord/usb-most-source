//! Real authenticated TCP, product agent and RAM USB fixture; OS removal is
//! simulated here. Separate Windows/Linux gates exercise actual OS adapters.
use super::*;
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{Mutex, mpsc},
};
use usb_most_protocol::{eject::State, wire::Frame};
#[path = "../../../../tests/fixtures/managed_usb.rs"]
pub(crate) mod managed_fixture;
#[path = "../../../../tests/fixtures/usb_storage.rs"]
pub(crate) mod storage;

struct SimulatedOs {
    socket: TcpStream,
    calls: Arc<AtomicUsize>,
}
impl Attachment for SimulatedOs {
    fn finished(&self) -> bool {
        false
    }
    fn eject(&mut self) -> Outcome {
        if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            return Outcome::Busy;
        }
        self.socket.shutdown(Shutdown::Both).unwrap();
        Outcome::Safe
    }
}
impl Drop for SimulatedOs {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}
fn wait(ready: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "managed TCP fixture deadline"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn managed_import_keeps_metadata_out_of_usb_and_recovers_both_lost_messages() {
    let storage = storage::Fixture::start();
    let fixture = managed_fixture::Fixture::start(storage.address);
    let caps = fixture.capabilities();
    let remote = fixture.connector();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut kernel = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    kernel
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (local, _) = listener.accept().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    let proxy = thread::spawn(move || {
        agent::serve_managed(
            local,
            remote.as_ref(),
            &Mutex::new(agent::Policy::default()),
            None,
            &agent::Management {
                capabilities: caps,
                imported: &|key| {
                    tx.try_send(key)
                        .map_err(|_| io::ErrorKind::InvalidData.into())
                },
            },
        )
    });
    kernel.write_all(&[1, 17, 128, 3, 0, 0, 0, 0]).unwrap();
    let mut id = [0; 32];
    id[..3].copy_from_slice(b"1-8");
    kernel.write_all(&id).unwrap();
    let mut descriptor = [0; 320];
    kernel.read_exact(&mut descriptor).unwrap();
    assert_eq!(descriptor[..8], [1, 17, 0, 3, 0, 0, 0, 0]);
    let key = rx.try_recv().expect("managed key before kernel descriptor");
    assert_eq!(key.session, caps.session);
    let mut request = Frame::packet(1, 1, 0x10008, 1, 0, 0, 18, 0, &[]);
    request.0[40..48].copy_from_slice(&[0x80, 6, 0, 1, 0, 0, 18, 0]);
    request.send(&mut kernel).unwrap();
    let mut reply = [0; 66];
    kernel.read_exact(&mut reply).unwrap();
    assert_eq!(&reply[..4], &3u32.to_be_bytes());
    assert_eq!(&reply[24..28], &18u32.to_be_bytes());
    assert_eq!(&reply[48..50], &[18, 1]); // USB device descriptor, no UMM metadata
    let calls = Arc::new(AtomicUsize::new(0));
    let task = Managed::start(
        Permit::acquire().unwrap(),
        SimulatedOs {
            socket: kernel,
            calls: calls.clone(),
        },
        key,
        fixture.connector(),
    )
    .unwrap();
    wait(|| fixture.ready(key));
    assert_eq!(fixture.request(key), 1);
    wait(|| {
        fixture.state(key)
            == Some(State::Completed {
                sequence: 1,
                outcome: Outcome::Busy,
            })
    });
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(fixture.request(key), 2); // new explicit UI retry after Busy
    wait(|| {
        fixture.state(key)
            == Some(State::Completed {
                sequence: 2,
                outcome: Outcome::Safe,
            })
            && fixture.losses() == (1, 1)
    });
    assert!(!task.finished());
    assert_eq!(calls.load(Ordering::Acquire), 2);
    assert_eq!(storage.counts.imports.load(Ordering::Acquire), 1);
    fixture.acknowledge(key);
    wait(|| task.finished());
    proxy.join().unwrap().unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 2);
}
