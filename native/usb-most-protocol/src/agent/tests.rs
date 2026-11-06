use super::*;
#[test]
fn managed_commands_do_not_open_an_unauthenticated_connection() {
    struct Plain;
    impl Connector for Plain {
        fn connect(&self) -> io::Result<TcpStream> {
            panic!("plain transport must never see management traffic")
        }
    }
    assert!(management_capabilities(&Plain).unwrap().is_none());
    let report = crate::managed::Report {
        import: crate::eject::ImportKey {
            session: [1; 16],
            number: 1,
        },
        state: crate::eject::State::Idle,
    };
    assert_eq!(
        sync_ejection(&Plain, report).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn managed_metadata_is_bounded_and_cannot_cross_imports() {
    use crate::{
        eject::{ImportKey, State},
        managed::{self, Action, Capabilities, Directive, Report},
    };
    struct Secure(Mutex<Option<TcpStream>>);
    impl Connector for Secure {
        fn connect(&self) -> io::Result<TcpStream> {
            self.0.lock().unwrap().take().ok_or_else(invalid)
        }
        fn authenticated(&self) -> bool {
            true
        }
    }
    let key = ImportKey {
        session: [17; 16],
        number: 1,
    };
    let caps = Capabilities {
        session: key.session,
        features: managed::GENERIC | managed::EJECT,
        max_imports: 4,
    };
    let (client, mut peer) = pair();
    let server = thread::spawn(move || {
        let mut header = [0; 8];
        peer.read_exact(&mut header).unwrap();
        assert_eq!(&header[2..4], &managed::CAPS.to_be_bytes());
        peer.write_all(&[1, 17, 0, 0xe1, 0, 0, 0, 0]).unwrap();
        peer.write_all(&caps.encode().unwrap()).unwrap();
    });
    assert_eq!(
        management_capabilities(&Secure(Mutex::new(Some(client)))).unwrap(),
        Some(caps)
    );
    server.join().unwrap();
    for wrong_key in [false, true] {
        let (client, mut peer) = pair();
        let report = Report {
            import: key,
            state: State::Idle,
        };
        let server = thread::spawn(move || {
            let mut header = [0; 8];
            peer.read_exact(&mut header).unwrap();
            assert_eq!(&header[2..4], &managed::SYNC.to_be_bytes());
            let mut body = [0; 40];
            peer.read_exact(&mut body).unwrap();
            assert_eq!(Report::decode(&body).unwrap(), report);
            let directive = Directive {
                import: ImportKey {
                    number: if wrong_key { 2 } else { 1 },
                    ..key
                },
                sequence: 1,
                action: Action::Eject,
            };
            peer.write_all(&[1, 17, 0, 0xe3, 0, 0, 0, 0]).unwrap();
            peer.write_all(&directive.encode().unwrap()).unwrap();
        });
        let result = sync_ejection(&Secure(Mutex::new(Some(client))), report);
        assert_eq!(result.is_err(), wrong_key);
        server.join().unwrap();
    }
    // A partial reply and a stale operation are not transformed into OS success.
    let (client, mut peer) = pair();
    let server = thread::spawn(move || {
        let mut request = [0; 48];
        peer.read_exact(&mut request).unwrap();
        peer.write_all(&[1, 17, 0, 0xe3, 0, 0, 0, 0, 0]).unwrap();
    });
    assert!(
        sync_ejection(
            &Secure(Mutex::new(Some(client))),
            Report {
                import: key,
                state: State::Idle
            }
        )
        .is_err()
    );
    server.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn managed_import_metadata_never_reaches_kernel_or_replaced_session() {
    use crate::{
        eject::ImportKey,
        managed::{self, Capabilities},
    };
    struct Secure(Mutex<VecDeque<TcpStream>>);
    impl Connector for Secure {
        fn connect(&self) -> io::Result<TcpStream> {
            self.0.lock().unwrap().pop_front().ok_or_else(invalid)
        }
        fn authenticated(&self) -> bool {
            true
        }
    }
    for case in 0..5 {
        let (list_client, mut list) = pair();
        let (import_client, mut imported) = pair();
        let (mut kernel, local) = pair();
        kernel
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        imported
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let key = ImportKey {
            session: [7; 16],
            number: 19,
        };
        let caps = Capabilities {
            session: key.session,
            features: managed::GENERIC | managed::EJECT,
            max_imports: 4,
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let agent = thread::spawn(move || {
            serve_managed(
                local,
                &Secure(Mutex::new(VecDeque::from([list_client, import_client]))),
                &Mutex::new(Policy::default()),
                None,
                &Management {
                    capabilities: caps,
                    imported: &|actual| {
                        captured.lock().unwrap().push(actual);
                        if case == 4 {
                            Err(io::ErrorKind::Other.into())
                        } else {
                            Ok(())
                        }
                    },
                },
            )
        });
        let peer = thread::spawn(move || {
            let mut d = [0; 312];
            d[256..259].copy_from_slice(b"1-7");
            d[292..296].copy_from_slice(&7u32.to_be_bytes());
            d[296..300].copy_from_slice(&3u32.to_be_bytes());
            let mut h = [0; 8];
            list.read_exact(&mut h).unwrap();
            assert_eq!(&h[2..4], &0x8005u16.to_be_bytes());
            list.write_all(&[1, 17, 0, 5, 0, 0, 0, 0, 0, 0, 0, 1])
                .unwrap();
            list.write_all(&d).unwrap();
            drop(list);
            imported.read_exact(&mut h).unwrap();
            assert_eq!(&h[2..4], &managed::IMPORT.to_be_bytes());
            let mut body = [0; 52];
            imported.read_exact(&mut body).unwrap();
            let request = managed::ImportRequest::decode(&body).unwrap();
            assert_eq!(request.session, key.session);
            assert!(!request.wan);
            assert_eq!(&request.busid[..4], b"1-7\0");
            imported.write_all(&[1, 17, 0, 0xe2, 0, 0, 0, 0]).unwrap();
            if case == 3 {
                d[258] = b'8';
            }
            let actual = ImportKey {
                session: if case == 1 { [8; 16] } else { key.session },
                ..key
            };
            let mut response = d.to_vec();
            response.extend(managed::encode_key(actual).unwrap());
            if case == 2 {
                response.pop();
            }
            imported.write_all(&response).unwrap();
            if case == 0 {
                let r = read(&mut imported);
                assert_eq!(r.command(), 1);
                Frame::response(&r, 0, &[42, 43])
                    .send(&mut imported)
                    .unwrap();
            }
        });
        request(&mut kernel, 0x8003).unwrap();
        let mut name = [0; 32];
        name[..3].copy_from_slice(b"1-7");
        kernel.write_all(&name).unwrap();
        let mut response = [0; 320];
        if case == 0 {
            kernel.read_exact(&mut response).unwrap();
            assert_eq!(&response[264..296], &name);
            input(1).send(&mut kernel).unwrap();
            assert_eq!(read(&mut kernel).data(), [42, 43]);
            kernel.shutdown(Shutdown::Both).unwrap();
            assert!(agent.join().unwrap().is_ok());
        } else {
            assert!(kernel.read_exact(&mut response).is_err());
            assert!(agent.join().unwrap().is_err());
        }
        peer.join().unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            if matches!(case, 0 | 4) {
                vec![key]
            } else {
                vec![]
            }
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn managed_wan_and_explicit_preclaim_fallback_keep_exact_metadata() {
    use crate::{
        eject::ImportKey,
        managed::{self, Capabilities},
    };
    struct Secure(Mutex<VecDeque<TcpStream>>);
    impl Connector for Secure {
        fn connect(&self) -> io::Result<TcpStream> {
            self.0.lock().unwrap().pop_front().ok_or_else(invalid)
        }
        fn authenticated(&self) -> bool {
            true
        }
    }
    for fallback in [false, true] {
        let (list_client, mut list) = pair();
        let probes: Vec<_> = (0..3).map(|_| pair()).collect();
        let (import_client, mut imported) = pair();
        let (fallback_client, mut generic) = pair();
        let (mut kernel, local) = pair();
        kernel
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        imported
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        generic
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut client = VecDeque::from([list_client]);
        let mut peer_probes = Vec::new();
        for (a, b) in probes {
            client.push_back(a);
            peer_probes.push(b);
        }
        client.push_back(import_client);
        if fallback {
            client.push_back(fallback_client)
        } else {
            drop(fallback_client)
        };
        let key = ImportKey {
            session: [21; 16],
            number: 3,
        };
        let seen = Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let agent = thread::spawn(move || {
            serve_managed(
                local,
                &Secure(Mutex::new(client)),
                &Mutex::new(Policy::default()),
                None,
                &Management {
                    capabilities: Capabilities {
                        session: key.session,
                        features: 7,
                        max_imports: 4,
                    },
                    imported: &|key| {
                        *captured.lock().unwrap() = Some(key);
                        Ok(())
                    },
                },
            )
        });
        let peer = thread::spawn(move || {
            let wan = [0x57, 0x41, 0x4e, 0x31, 0, 0, 8, 0, 0, 0, 0, 32, 0, 0, 0, 1];
            let mut d = [0; 312];
            d[256..259].copy_from_slice(b"1-7");
            d[292..296].copy_from_slice(&7u32.to_be_bytes());
            d[296..300].copy_from_slice(&3u32.to_be_bytes());
            d[300..304].copy_from_slice(&[4, 109, 197, 43]);
            let mut h = [0; 8];
            list.read_exact(&mut h).unwrap();
            list.write_all(&[1, 17, 0, 5, 0, 0, 0, 0, 0, 0, 0, 1])
                .unwrap();
            list.write_all(&d).unwrap();
            drop(list);
            for mut probe in peer_probes {
                probe.read_exact(&mut h).unwrap();
                assert_eq!(&h[2..4], &0x80dfu16.to_be_bytes());
                thread::sleep(Duration::from_millis(65));
                probe.write_all(&[1, 17, 0, 0xdf, 0, 0, 0, 0]).unwrap();
                probe.write_all(&wan).unwrap();
            }
            imported.read_exact(&mut h).unwrap();
            assert_eq!(&h[2..4], &managed::IMPORT.to_be_bytes());
            let mut body = [0; 52];
            imported.read_exact(&mut body).unwrap();
            assert!(managed::ImportRequest::decode(&body).unwrap().wan);
            let active = if fallback {
                imported.write_all(&[1, 17, 0, 0xe2, 0, 0, 0, 95]).unwrap();
                drop(imported);
                generic.read_exact(&mut h).unwrap();
                assert_eq!(&h[2..4], &managed::IMPORT.to_be_bytes());
                generic.read_exact(&mut body).unwrap();
                let request = managed::ImportRequest::decode(&body).unwrap();
                assert!(!request.wan);
                assert_eq!(request.session, key.session);
                &mut generic
            } else {
                &mut imported
            };
            active.write_all(&[1, 17, 0, 0xe2, 0, 0, 0, 0]).unwrap();
            active.write_all(&d).unwrap();
            active
                .write_all(&managed::encode_key(key).unwrap())
                .unwrap();
            if !fallback {
                active.write_all(&wan).unwrap();
            }
            if fallback {
                let r = read(active);
                Frame::response(&r, 0, &[91]).send(active).unwrap();
            } else {
                assert_eq!(read(active).command(), START);
                Frame::packet(PUSH, 1, 7, 1, 3, 0, 1, 1, &[91])
                    .send(active)
                    .unwrap();
                assert_eq!(read(active).command(), CREDIT);
            }
        });
        request(&mut kernel, 0x8003).unwrap();
        let mut name = [0; 32];
        name[..3].copy_from_slice(b"1-7");
        kernel.write_all(&name).unwrap();
        let mut response = [0; 320];
        kernel.read_exact(&mut response).unwrap();
        input(1).send(&mut kernel).unwrap();
        assert_eq!(read(&mut kernel).data(), [91]);
        kernel.shutdown(Shutdown::Both).unwrap();
        let metrics = agent.join().unwrap().unwrap();
        assert_eq!(
            metrics.mode,
            if fallback { Mode::Generic } else { Mode::Wan }
        );
        peer.join().unwrap();
        assert_eq!(*seen.lock().unwrap(), Some(key));
    }
}
fn pair() -> (TcpStream, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let a = TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (b, _) = l.accept().unwrap();
    (a, b)
}
fn read(s: &mut TcpStream) -> Frame {
    Frame::read(s).unwrap()
}
fn input(seq: u32) -> Frame {
    Frame::packet(1, seq, 7, 1, 3, 0, 32, 0, &[])
}
#[test]
#[cfg_attr(miri, ignore)]
fn large_bulk_frames_and_byte_budget_recover_without_disconnect_or_replay() {
    let (mut kernel, local) = pair();
    let (remote, mut android) = pair();
    let stop = kernel.try_clone().unwrap();
    let peer_stop = android.try_clone().unwrap();
    let (done, deadline) = mpsc::channel();
    let guard = thread::spawn(move || {
        if deadline.recv_timeout(Duration::from_secs(15)).is_err() {
            let _ = stop.shutdown(Shutdown::Both);
            let _ = peer_stop.shutdown(Shutdown::Both);
        }
    });
    let agent = thread::spawn(move || relay(local, remote, 7, Mode::Generic));
    for seq in 1..=8 {
        Frame::packet(1, seq, 7, 1, 2, 0, wire::MAX as u32, 0, &[])
            .send(&mut kernel)
            .unwrap();
        let forwarded = read(&mut android);
        assert_eq!(
            (forwarded.word(4), forwarded.word(24)),
            (seq, wire::MAX as u32)
        );
    }
    let refused = |kernel: &mut TcpStream, android: &mut TcpStream, seq| {
        Frame::packet(1, seq, 7, 0, 2, 0, 1, 0, &[73])
            .send(kernel)
            .unwrap();
        let reply = read(kernel);
        assert_eq!(
            (
                reply.command(),
                reply.word(4),
                reply.word(20) as i32,
                reply.word(24)
            ),
            (3, seq, -12, 0)
        );
        android
            .set_read_timeout(Some(Duration::from_millis(30)))
            .unwrap();
        let error = android.peek(&mut [0]).unwrap_err().kind();
        assert!(matches!(
            error,
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    };
    refused(&mut kernel, &mut android, 9);
    // UNLINK request alone cannot free the in-flight reservation.
    Frame::packet(2, 10, 7, 1, 2, 1, 0, 0, &[])
        .send(&mut kernel)
        .unwrap();
    assert_eq!(read(&mut android).command(), 2);
    refused(&mut kernel, &mut android, 11);
    Frame::packet(4, 10, 7, 1, 2, -104, 0, 0, &[])
        .send(&mut android)
        .unwrap();
    assert_eq!(read(&mut kernel).command(), 4);
    let data: Vec<_> = (0..wire::MAX).map(|i| (i ^ (i >> 8)) as u8).collect();
    Frame::packet(1, 12, 7, 0, 2, 0, wire::MAX as u32, 0, &data)
        .send(&mut kernel)
        .unwrap();
    let forwarded = read(&mut android);
    assert_eq!(forwarded.word(4), 12);
    assert_eq!(forwarded.data(), data);
    // Partial/error input still preserves every byte and refunds full capacity.
    Frame::packet(3, 2, 7, 1, 2, -110, 520192, 0, &data[..520192])
        .send(&mut android)
        .unwrap();
    let reply = read(&mut kernel);
    assert_eq!(reply.word(20) as i32, -110);
    assert_eq!(reply.data(), &data[..520192]);
    Frame::packet(1, 13, 7, 1, 2, 0, wire::MAX as u32, 0, &[])
        .send(&mut kernel)
        .unwrap();
    assert_eq!(read(&mut android).word(4), 13);
    Frame::packet(3, 13, 7, 1, 2, 0, wire::MAX as u32, 0, &data)
        .send(&mut android)
        .unwrap();
    assert_eq!(read(&mut kernel).data(), data);
    // OUT completion must not cause a second execution, including an error.
    Frame::packet(3, 12, 7, 0, 2, -110, 520192, 0, &[])
        .send(&mut android)
        .unwrap();
    assert_eq!(read(&mut kernel).word(24), 520192);
    android
        .set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    assert!(matches!(
        android.peek(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    kernel.shutdown(Shutdown::Both).unwrap();
    agent.join().unwrap().unwrap();
    done.send(()).unwrap();
    guard.join().unwrap();
}
#[test]
fn automatic_choice_is_capability_gated_and_stable() {
    let mut p = Policy::default();
    for ms in [0, 20, 40, 59] {
        assert_eq!(
            p.choose("1-6", true, Duration::from_millis(ms)),
            Mode::Generic
        );
    }
    assert_eq!(p.choose("1-6", true, Duration::from_millis(60)), Mode::Wan);
    assert_eq!(p.choose("1-6", true, Duration::from_millis(40)), Mode::Wan);
    assert_eq!(
        p.choose("1-6", false, Duration::from_secs(2)),
        Mode::Generic
    );
    assert_eq!(
        p.choose("1-6", true, Duration::from_millis(30)),
        Mode::Generic
    );
}
#[test]
#[cfg_attr(miri, ignore)]
fn push_cancel_barrier_and_exactly_once_control() {
    let (mut kernel, local) = pair();
    let (remote, mut tablet) = pair();
    let stop = kernel.try_clone().unwrap();
    let tstop = tablet.try_clone().unwrap();
    let (done, deadline) = mpsc::channel();
    let guard = thread::spawn(move || {
        if deadline.recv_timeout(Duration::from_secs(10)).is_err() {
            let _ = stop.shutdown(Shutdown::Both);
            let _ = tstop.shutdown(Shutdown::Both);
        }
    });
    let agent = thread::spawn(move || relay(local, remote, 7, Mode::Wan));
    input(1).send(&mut kernel).unwrap();
    assert_eq!(read(&mut tablet).command(), START);
    Frame::packet(PUSH, 1, 7, 1, 3, 0, 4, 1, &[0x20, 1, 2, 1])
        .send(&mut tablet)
        .unwrap();
    let r = read(&mut kernel);
    assert_eq!(r.word(4), 1);
    assert_eq!(r.data(), [0x20, 1, 2, 1]);
    assert_eq!(read(&mut tablet).word(24), 1);
    input(2).send(&mut kernel).unwrap();
    Frame::packet(2, 3, 7, 1, 3, 2, 0, 0, &[])
        .send(&mut kernel)
        .unwrap();
    assert_eq!(read(&mut kernel).word(20) as i32, -104);
    Frame::packet(PUSH, 2, 7, 1, 3, 0, 1, 1, &[42])
        .send(&mut tablet)
        .unwrap();
    input(4).send(&mut kernel).unwrap();
    assert_eq!(read(&mut kernel).data(), [42]);
    assert_eq!(read(&mut tablet).word(24), 2);
    let mut configure = Frame::packet(1, 10, 7, 0, 0, 0, 0, 0, &[]);
    configure.0[40..48].copy_from_slice(&[0, 9, 1, 0, 0, 0, 0, 0]);
    configure.send(&mut kernel).unwrap();
    assert_eq!(read(&mut tablet).word(4), 10);
    Frame::packet(BARRIER, 0, 7, 1, 3, 0, 0, 2, &[])
        .send(&mut tablet)
        .unwrap();
    Frame::response(&configure, 0, &[])
        .send(&mut tablet)
        .unwrap();
    assert_eq!(read(&mut kernel).word(4), 10);
    input(11).send(&mut kernel).unwrap();
    let start = read(&mut tablet);
    assert_eq!((start.command(), start.word(28)), (START, 2));
    Frame::packet(PUSH, 1, 7, 1, 3, 0, 1, 2, &[99])
        .send(&mut tablet)
        .unwrap();
    assert_eq!(read(&mut kernel).data(), [99]);
    assert_eq!(read(&mut tablet).word(24), 1);
    kernel.shutdown(Shutdown::Both).unwrap();
    let metrics = agent.join().unwrap().unwrap();
    assert_eq!(
        (metrics.delivered, metrics.controls, metrics.cancels),
        (3, 1, 1)
    );
    done.send(()).unwrap();
    guard.join().unwrap();
}
#[test]
#[cfg_attr(miri, ignore)]
fn stale_epoch_and_duplicates_fail_closed() {
    for (seq, epoch) in [(2, 1), (1, 2)] {
        let (mut kernel, local) = pair();
        let (remote, mut tablet) = pair();
        let agent = thread::spawn(move || relay(local, remote, 7, Mode::Wan));
        input(1).send(&mut kernel).unwrap();
        read(&mut tablet);
        Frame::packet(PUSH, seq, 7, 1, 3, 0, 1, epoch, &[1])
            .send(&mut tablet)
            .unwrap();
        assert!(agent.join().unwrap().is_err());
    }
}
#[test]
fn bounded_headers_reject_payload_bombs() {
    for command in [1u32, 3, 16] {
        for size in [1048577u32, u32::MAX] {
            let f = Frame::packet(command, 1, 7, 1, 3, 0, size, 1, &[]);
            let h: &[u8; 48] = f.0.as_slice().try_into().unwrap();
            assert!(wire::body_size(h).is_err());
        }
    }
    let c = Frame::control(CREDIT, 7, 1, 1_000_000);
    assert_eq!(
        wire::body_size(c.0.as_slice().try_into().unwrap()).unwrap(),
        0
    );
}
#[test]
#[cfg_attr(miri, ignore)]
fn rejected_configuration_keeps_input_epoch_and_buffer() {
    let (mut kernel, local) = pair();
    let (remote, mut tablet) = pair();
    let task = thread::spawn(move || relay(local, remote, 7, Mode::Wan));
    input(1).send(&mut kernel).unwrap();
    read(&mut tablet);
    Frame::packet(PUSH, 1, 7, 1, 3, 0, 1, 1, &[1])
        .send(&mut tablet)
        .unwrap();
    read(&mut kernel);
    read(&mut tablet);
    let mut cfg = Frame::packet(1, 2, 7, 0, 0, 0, 0, 0, &[]);
    cfg.0[40..48].copy_from_slice(&[0, 9, 99, 0, 0, 0, 0, 0]);
    cfg.send(&mut kernel).unwrap();
    read(&mut tablet);
    Frame::packet(PUSH, 2, 7, 1, 3, 0, 1, 1, &[2])
        .send(&mut tablet)
        .unwrap();
    Frame::response(&cfg, -22, &[]).send(&mut tablet).unwrap();
    assert_eq!(read(&mut kernel).word(20) as i32, -22);
    input(3).send(&mut kernel).unwrap();
    assert_eq!(read(&mut kernel).data(), [2]);
    assert_eq!(read(&mut tablet).word(24), 2);
    kernel.shutdown(Shutdown::Both).unwrap();
    task.join().unwrap().unwrap();
}
#[test]
fn every_wan_reserved_word_is_validated() {
    for cmd in [PUSH, START, CREDIT, BARRIER] {
        let f = if cmd == PUSH {
            Frame::packet(cmd, 1, 7, 1, 3, 0, 0, 1, &[])
        } else if cmd == BARRIER {
            Frame::packet(cmd, 0, 7, 1, 3, 0, 0, 1, &[])
        } else {
            Frame::control(cmd, 7, 1, u32::from(cmd == CREDIT))
        };
        for at in [32, 36, 40, 44] {
            let mut h: [u8; 48] = f.0.as_slice().try_into().unwrap();
            h[at] = 1;
            assert!(wire::body_size(&h).is_err());
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn idle_session_outlives_handshake_deadlines() {
    let (mut kernel, local) = pair();
    let (remote, mut tablet) = pair();
    for s in [&local, &remote] {
        s.set_read_timeout(Some(Duration::from_millis(30))).unwrap();
    }
    for s in [&kernel, &tablet] {
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    }
    let task = thread::spawn(move || relay(local, remote, 7, Mode::Generic));
    thread::sleep(Duration::from_millis(150));
    let request = Frame::packet(1, 1, 7, 0, 0, 0, 0, 0, &[]);
    request.send(&mut kernel).unwrap();
    assert_eq!(read(&mut tablet).0, request.0);
    Frame::response(&request, 0, &[]).send(&mut tablet).unwrap();
    assert_eq!(read(&mut kernel).word(4), 1);
    kernel.shutdown(Shutdown::Both).unwrap();
    assert_eq!(task.join().unwrap().unwrap().controls, 1);
}

#[test]
#[cfg_attr(miri, ignore)]
fn only_preclaim_unsupported_profile_falls_back_without_user_action() {
    for rejected in [95u32, 1] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let fixture = thread::spawn(move || {
            let mut device = [0u8; 312];
            device[256..259].copy_from_slice(b"1-6");
            device[291] = 1;
            device[295] = 6;
            device[300..304].copy_from_slice(&[4, 109, 197, 43]);
            let mut seen = Vec::new();
            for _ in 0..if rejected == 95 { 6 } else { 5 } {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut h = [0; 8];
                socket.read_exact(&mut h).unwrap();
                let op = u16::from_be_bytes([h[2], h[3]]);
                seen.push(op);
                match op {
                    0x8005 => {
                        socket
                            .write_all(&[1, 17, 0, 5, 0, 0, 0, 0, 0, 0, 0, 1])
                            .unwrap();
                        socket.write_all(&device).unwrap();
                    }
                    0x80df => {
                        thread::sleep(Duration::from_millis(80));
                        socket.write_all(&[1, 17, 0, 223, 0, 0, 0, 0]).unwrap();
                        socket
                            .write_all(&[
                                0x57, 0x41, 0x4e, 0x31, 0, 0, 8, 0, 0, 0, 0, 32, 0, 0, 0, 1,
                            ])
                            .unwrap();
                    }
                    0x80e0 => {
                        let mut body = [0; 36];
                        socket.read_exact(&mut body).unwrap();
                        assert_eq!(&body[..4], b"1-6\0");
                        assert_eq!(wire::u32_at(&body, 32), 1);
                        socket.write_all(&[1, 17, 0, 224]).unwrap();
                        socket.write_all(&rejected.to_be_bytes()).unwrap();
                    }
                    0x8003 => {
                        let mut body = [0; 32];
                        socket.read_exact(&mut body).unwrap();
                        socket.write_all(&[1, 17, 0, 3, 0, 0, 0, 0]).unwrap();
                        socket.write_all(&device).unwrap();
                        let control = read(&mut socket);
                        Frame::response(&control, 0, &[]).send(&mut socket).unwrap();
                        assert!(Frame::read(&mut socket).is_err());
                    }
                    _ => panic!("unexpected negotiation operation"),
                }
            }
            seen
        });
        let (mut kernel, local) = pair();
        kernel
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let task = thread::spawn(move || serve(local, target, &Mutex::new(Policy::default())));
        kernel.write_all(&[1, 17, 128, 3, 0, 0, 0, 0]).unwrap();
        let mut bus = [0; 32];
        bus[..3].copy_from_slice(b"1-6");
        kernel.write_all(&bus).unwrap();
        assert_eq!(
            reply(&mut kernel, 3).unwrap(),
            if rejected == 95 { 0 } else { 1 }
        );
        if rejected == 95 {
            let mut device = [0; 312];
            kernel.read_exact(&mut device).unwrap();
            let control = Frame::packet(1, 1, 65542, 0, 0, 0, 0, 0, &[]);
            control.send(&mut kernel).unwrap();
            assert_eq!(read(&mut kernel).word(4), 1);
            kernel.shutdown(Shutdown::Both).unwrap();
            assert_eq!(task.join().unwrap().unwrap().mode, Mode::Generic);
        } else {
            assert_eq!(task.join().unwrap().unwrap().mode, Mode::Wan);
        }
        let seen = fixture.join().unwrap();
        assert_eq!(&seen[..5], &[0x8005, 0x80df, 0x80df, 0x80df, 0x80e0]);
        assert_eq!(seen.len(), if rejected == 95 { 6 } else { 5 });
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn kernel_alias_rejects_enumeration_guesses_and_replay_before_remote_access() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Denied(AtomicUsize);
    impl Connector for Denied {
        fn connect(&self) -> io::Result<TcpStream> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Err(io::ErrorKind::NotConnected.into())
        }
    }
    let remote = Denied(AtomicUsize::new(0));
    let policy = Mutex::new(Policy::default());
    let alias = ImportAlias::new("0123456789abcdef0123456789abcd", "1-7").unwrap();
    for op in [0x8005u16, 0x8003, 0x80e0] {
        let (mut kernel, local) = pair();
        request(&mut kernel, op).unwrap();
        kernel.write_all(&[0u8; 32]).unwrap();
        assert!(serve_alias(local, &remote, &policy, &alias).is_err());
        assert_eq!(remote.0.load(Ordering::Acquire), 0);
    }
    let (mut kernel, local) = pair();
    request(&mut kernel, 0x8003).unwrap();
    kernel.write_all(alias.encoded()).unwrap();
    assert!(serve_alias(local, &remote, &policy, &alias).is_err());
    assert_eq!(remote.0.load(Ordering::Acquire), 1);
    let (mut kernel, local) = pair();
    request(&mut kernel, 0x8003).unwrap();
    kernel.write_all(alias.encoded()).unwrap();
    assert!(serve_alias(local, &remote, &policy, &alias).is_err());
    assert_eq!(remote.0.load(Ordering::Acquire), 1);
}

#[test]
#[cfg_attr(miri, ignore)]
fn kernel_alias_translates_only_import_name_and_preserves_device_and_transfer_bytes() {
    struct Scripted(Mutex<VecDeque<TcpStream>>);
    impl Connector for Scripted {
        fn connect(&self) -> io::Result<TcpStream> {
            self.0.lock().unwrap().pop_front().ok_or_else(invalid)
        }
    }
    let (list_client, mut list) = pair();
    let (import_client, mut imported) = pair();
    let (mut kernel, local) = pair();
    kernel
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    imported
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    list.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let alias = Arc::new(ImportAlias::new("0123456789abcdef0123456789abcd", "1-7").unwrap());
    let agent_alias = alias.clone();
    let agent = thread::spawn(move || {
        serve_alias(
            local,
            &Scripted(Mutex::new(VecDeque::from([list_client, import_client]))),
            &Mutex::new(Policy::default()),
            &agent_alias,
        )
    });
    let tablet = thread::spawn(move || {
        let mut descriptor = [0; 312];
        descriptor[256..259].copy_from_slice(b"1-7");
        descriptor[292..296].copy_from_slice(&7u32.to_be_bytes());
        descriptor[296..300].copy_from_slice(&3u32.to_be_bytes());
        let mut header = [0; 8];
        list.read_exact(&mut header).unwrap();
        assert_eq!(&header[2..4], &0x8005u16.to_be_bytes());
        list.write_all(&[1, 17, 0, 5, 0, 0, 0, 0, 0, 0, 0, 1])
            .unwrap();
        list.write_all(&descriptor).unwrap();
        drop(list);
        imported.read_exact(&mut header).unwrap();
        assert_eq!(&header[2..4], &0x8003u16.to_be_bytes());
        let mut name = [0; 32];
        imported.read_exact(&mut name).unwrap();
        assert_eq!(&name[..4], b"1-7\0");
        imported.write_all(&[1, 17, 0, 3, 0, 0, 0, 0]).unwrap();
        imported.write_all(&descriptor).unwrap();
        let request = read(&mut imported);
        Frame::response(&request, 0, &[1, 2, 3])
            .send(&mut imported)
            .unwrap();
    });
    request(&mut kernel, 0x8003).unwrap();
    kernel.write_all(alias.encoded()).unwrap();
    let mut response = [0; 320];
    kernel.read_exact(&mut response).unwrap();
    assert_eq!(&response[264..296], alias.encoded());
    assert_eq!(wire::u32_at(&response, 300), 7);
    input(1).send(&mut kernel).unwrap();
    assert_eq!(read(&mut kernel).data(), [1, 2, 3]);
    kernel.shutdown(Shutdown::Both).unwrap();
    tablet.join().unwrap();
    let _ = agent.join().unwrap();
}
