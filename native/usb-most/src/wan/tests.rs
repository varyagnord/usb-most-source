use super::sim::Fake;
use super::*;
use std::sync::Arc;
fn pump() -> (Arc<Fake>, Session<Arc<Fake>>) {
    let f = Arc::new(Fake::default());
    let s = Session::new(f.clone());
    s.command(1, 1).unwrap();
    s.command(5, 0).unwrap();
    (f, s)
}
#[test]
fn independent_polling_credits_and_exact_data() {
    let (f, s) = pump();
    for n in 0..WINDOW as u32 {
        assert!(f.emit(&n.to_be_bytes()));
        let c = s.reap(0).unwrap().unwrap();
        assert_eq!(c.wan_sequence, n + 1);
        assert_eq!(c.data().unwrap(), n.to_be_bytes());
    }
    assert!(f.emit(&[99]));
    assert!(s.reap(0).unwrap().is_none());
    assert_eq!(s.command(4, 0).unwrap()[4], 1);
    assert!(s.command(2, WINDOW as i32 + 1).is_err());
    s.command(2, WINDOW as i32).unwrap();
    s.command(2, WINDOW as i32).unwrap();
    let c = s.reap(0).unwrap().unwrap();
    assert_eq!(c.data().unwrap(), [99]);
    assert_eq!(c.wan_sequence, WINDOW as u32 + 1);
    assert!(s.command(2, WINDOW as i32 - 1).is_err());
    assert_eq!(s.close(), 0);
}
#[test]
fn barrier_cancels_retires_and_invalidates_old_reports() {
    let (f, s) = pump();
    f.emit(&[1]);
    s.reap(0).unwrap();
    let stats = s.command(3, 0).unwrap();
    assert_eq!(stats[1], 2);
    assert_eq!(stats[3], 0);
    s.command(5, 0).unwrap();
    f.emit(&[2]);
    let c = s.reap(0).unwrap().unwrap();
    assert_eq!((c.wan_epoch, c.wan_sequence), (2, 1));
    assert_eq!(c.data().unwrap(), [2]);
    assert_eq!(s.close(), 0);
}
#[test]
fn overflow_fails_explicitly_without_unbounded_queue() {
    let (f, s) = pump();
    for _ in 0..WINDOW {
        f.emit(&[1]);
        s.reap(0).unwrap();
    }
    for _ in 0..WINDOW - 1 {
        assert!(f.emit(&[1]));
        assert!(s.reap(0).unwrap().is_none());
    }
    assert!(f.emit(&[1]));
    assert!(matches!(s.reap(0), Err(libc::ENOBUFS)));
    assert_eq!(s.close(), 0);
}
#[test]
fn logical_ids_never_alias_input_and_commands_never_replay() {
    let (f, s) = pump();
    for id in [0, -1, i32::MIN, i32::MAX, 1] {
        s.submit(
            Spec {
                id,
                kind: 2,
                endpoint: 0,
                flags: 0,
                length: 0,
                setup: Some([0; 8]),
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(s.command(3, 0).unwrap_err(), libc::EBUSY);
        let c = s.reap(0).unwrap().unwrap();
        assert_eq!(c.id, id);
        assert_eq!(c.wan_epoch, 0);
    }
    assert_eq!(f.controls(), 5);
    assert_eq!(s.cancel(100).unwrap_err(), libc::ENOENT);
    assert_eq!(s.close(), 0);
}
#[test]
fn high_latency_jitter_tcp_retransmission_matrix() {
    let counts = if cfg!(miri) { vec![8] } else { vec![128, 7600] };
    for count in counts {
        for rtt in [0, 20, 50, 100, 200, 350, 500, 1000, 2000] {
            for jitter in [0, 100, 500] {
                for loss in [0, 1, 10, 50] {
                    if cfg!(miri) && (jitter != 0 || loss != 0 || ![0, 350, 2000].contains(&rtt)) {
                        continue;
                    }
                    let s = super::lab::scenario(rtt, jitter, loss, count).unwrap_or_else(|e| {
                        panic!("rtt={rtt} jitter={jitter} loss={loss} count={count} error={e}")
                    });
                    assert_eq!(s.produced, s.delivered);
                    assert!(s.highwater < WINDOW as u64);
                    assert!(s.virtual_ms <= count as u64 * 8 + 30000);
                }
            }
        }
    }
}
#[test]
#[cfg_attr(miri, ignore)]
fn real_tcp_automatic_agent_with_delay() {
    for rtt in [0, 100, 500] {
        let stats = super::network_lab::scenario(rtt, 5, 16)
            .unwrap_or_else(|e| panic!("TCP RTT={rtt}: {e}"));
        assert_eq!(stats[0], 16);
        assert!(stats[2] < 30000);
    }
}
#[test]
fn retired_sample_lives_after_source_close() {
    let c = super::lab::sample(350, 100, 50, 8).unwrap();
    assert_eq!((c.wan_epoch, c.wan_sequence), (1, 1));
    assert_eq!(
        u64::from_be_bytes(c.data().unwrap()[..8].try_into().unwrap()),
        8
    );
}
