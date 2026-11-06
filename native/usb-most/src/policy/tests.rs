use super::*;
#[test]
fn connection_commands_are_bounded_canonical_and_deterministic() {
    assert_eq!(
        connection_commands("192.168.1.2", 1, &[1004, 1003]).unwrap(),
        "sudo modprobe vhci_hcd\nusbip list -r 192.168.1.2\nsudo usbip attach -r 192.168.1.2 -b 1-3\nsudo usbip attach -r 192.168.1.2 -b 1-4"
    );
    assert_eq!(
        connection_commands("100.64.0.1", 0, &[]).unwrap(),
        "sudo modprobe vhci_hcd\nusbip list -r 100.64.0.1"
    );
    for host in [
        "10.0.0.1;id",
        "$(id)",
        "10.0.0.1\nwhoami",
        "010.0.0.1",
        "8.8.8.8",
        "::1",
    ] {
        assert!(connection_commands(host, 1, &[1001]).is_none());
    }
    for ids in [
        &[-1][..],
        &[0],
        &[999],
        &[1000],
        &[2000],
        &[65536001],
        &[i32::MAX],
        &[1001, 1001],
    ] {
        assert!(connection_commands("10.0.0.1", 1, ids).is_none());
    }
    let ids: Vec<i32> = (1..=MAX_DEVICES as i32).map(|i| 1000 + i).collect();
    assert_eq!(
        connection_commands("10.0.0.1", 1, &ids)
            .unwrap()
            .lines()
            .count(),
        MAX_DEVICES + 2
    );
    assert!(connection_commands("10.0.0.1", 1, &[1001; MAX_DEVICES + 1]).is_none());
    assert!(connection_commands("10.0.0.1", 0, &[1001]).is_none());
}
#[test]
fn address_ranges_and_canonical_form() {
    for host in ["100.64.0.0", "100.64.0.1", "100.127.255.255"] {
        assert!(valid_host(host, 0));
        assert!(!valid_host(host, 1));
    }
    for host in [
        "10.0.0.1",
        "10.255.255.254",
        "172.16.0.1",
        "172.31.255.254",
        "192.168.0.1",
        "192.168.255.254",
    ] {
        assert!(valid_host(host, 1));
        assert!(!valid_host(host, 0));
    }
    for host in [
        "",
        " ",
        "192.168.1.1 ",
        " 10.0.0.1",
        "127.0.0.1",
        "0.0.0.0",
        "255.255.255.255",
        "8.8.8.8",
        "169.254.1.1",
        "172.15.255.255",
        "172.32.0.0",
        "100.63.255.255",
        "100.128.0.0",
        "10.0.0.256",
        "010.0.0.1",
        "10.0.0.01",
        "10.0.0.-1",
        "10.0.0.+1",
        "localhost",
        "10.1",
        "10.0.0.1:3240",
        "10.0.0.1/24",
        "::1",
        "١٠.٠.٠.١",
        "10.0.0.1\n",
        "10.0.0.1\0",
    ] {
        for mode in [0, 1, 2, -1] {
            assert!(!valid_host(host, mode), "{host:?} mode={mode}");
        }
    }
    assert!(!valid_host("10.0.0.1", 2));
}
#[test]
fn selection_hotplug_all_and_partial() {
    let mut s = Selection::default();
    assert!(s.sync(&[1, 1, 2, 1, 3, 0]));
    assert_eq!(s.snapshot(), [0]);
    assert!(s.select(1, true));
    assert_eq!(s.snapshot(), [0, 1]);
    s.select_all(true);
    assert_eq!(s.snapshot(), [1, 1, 2]);
    assert!(s.sync(&[1, 1, 2, 1, 3, 0, 4, 1]));
    assert_eq!(s.snapshot(), [1, 1, 2, 4]);
    assert!(s.select(2, false));
    assert_eq!(s.snapshot(), [0, 1, 4]);
    assert!(s.sync(&[1, 1, 2, 1, 3, 0, 4, 1, 5, 1]));
    assert_eq!(s.snapshot(), [0, 1, 4]);
    assert!(s.sync(&[2, 1, 3, 0, 4, 1, 5, 1]));
    assert!(s.sync(&[1, 1, 2, 1, 3, 0, 4, 1, 5, 1]));
    assert_eq!(s.snapshot(), [0, 4]);
    s.select_all(false);
    assert_eq!(s.snapshot(), [0]);
}
#[test]
fn selection_rejects_invalid_atomically_and_bounds() {
    let mut s = Selection::default();
    s.sync(&[1, 1]);
    s.select(1, true);
    for bad in [&[1][..], &[-1, 1], &[1, 2], &[1, 1, 1, 0]] {
        assert!(!s.sync(bad));
        assert_eq!(s.snapshot(), [0, 1]);
    }
    assert!(!s.select(2, true));
    assert!(s.sync(&[1, 0]));
    assert!(!s.select(1, true));
    assert_eq!(s.snapshot(), [0]);
    let all: Vec<i32> = (0..MAX_DEVICES as i32).flat_map(|n| [n, 1]).collect();
    assert!(s.sync(&all));
    s.select_all(true);
    assert_eq!(s.snapshot().len(), MAX_DEVICES + 1);
    let mut extra = all;
    extra.extend([MAX_DEVICES as i32, 1]);
    assert!(!s.sync(&extra));
    assert_eq!(s.snapshot().len(), MAX_DEVICES + 1);
}
#[test]
fn pending_storage_and_disappearing_eligible() {
    let mut s = Selection::default();
    s.select_all(true);
    s.sync(&[1, 1, 2, 0]);
    assert_eq!(s.snapshot(), [1, 1]);
    s.sync(&[1, 1, 2, 1]);
    assert_eq!(s.snapshot(), [1, 1, 2]);
    s.sync(&[1, 0, 2, 1]);
    assert_eq!(s.snapshot(), [1, 2]);
}
fn descriptors() -> Vec<u8> {
    let mut raw = vec![
        18, 1, 0, 2, 0, 0, 0, 64, 0x89, 0x0a, 0x25, 0, 0x34, 0x12, 1, 2, 0, 1,
    ];
    raw.extend([9, 2, 32, 0, 1, 1, 0, 0x80, 50]);
    raw.extend([9, 4, 0, 0, 2, 8, 6, 0x50, 0]);
    raw.extend([7, 5, 0x81, 2, 0, 2, 0]);
    raw.extend([7, 5, 2, 2, 0, 2, 0]);
    raw
}
#[test]
fn real_descriptor_shape_and_capabilities() {
    let raw = descriptors();
    assert_eq!(describe(&raw), Some([0x1234, 1, 1, 1, 0, 0, 0, 0]));
    let mut mouse = raw.clone();
    mouse[18 + 9 + 5] = 3;
    mouse[18 + 9 + 9 + 3] = 3;
    assert_eq!(describe(&mouse).unwrap()[7], 0);
    let mut iso = raw.clone();
    iso[18 + 9 + 9 + 3] = 1;
    assert_eq!(describe(&iso).unwrap()[7], 2);
    let mut hub = raw.clone();
    hub[4] = 9;
    assert_eq!(describe(&hub).unwrap()[7], 1);
    assert_eq!(capability(0, 1, 1, 4), 4);
    assert_eq!(capability(0, 0, 1, 0), 3);
}
#[test]
fn malformed_descriptors_and_every_truncation() {
    let raw = descriptors();
    for n in 0..raw.len() {
        assert!(describe(&raw[..n]).is_none(), "truncation {n}");
    }
    for (index, value) in [
        (0, 0),
        (1, 2),
        (17, 2),
        (18, 1),
        (20, 255),
        (22, 2),
        (27, 0),
        (30, 1),
        (36, 0),
        (45, 0x70),
    ] {
        let mut bad = raw.clone();
        bad[index] = value;
        assert!(describe(&bad).is_none(), "corrupt byte {index}");
    }
    let mut bad = raw.clone();
    bad[45] = 0x81;
    assert!(describe(&bad).is_none());
}
#[test]
fn multiple_configurations_and_alternatives() {
    let mut raw = descriptors();
    raw[17] = 2;
    let mut second = raw[18..].to_vec();
    second[5] = 2;
    raw.extend(second);
    assert_eq!(describe(&raw).unwrap()[1], 2);
    let mut raw = descriptors();
    raw[20] = 41;
    raw.extend([9, 4, 0, 1, 0, 8, 6, 0x50, 0]);
    assert!(describe(&raw).is_some());
    let mut bad = raw;
    *bad.last_mut().unwrap() = 0;
    bad[53] = 0;
    assert!(describe(&bad).is_none());
}
#[test]
fn deterministic_untrusted_descriptor_fuzz() {
    let mut seed = 17u64;
    let iterations = if cfg!(miri) { 80 } else { 10000 };
    for _ in 0..iterations {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let len = (seed % 512) as usize;
        let mut data = vec![0; len];
        for b in &mut data {
            seed = seed.rotate_left(7).wrapping_add(0x9e3779b9);
            *b = seed as u8;
        }
        let _ = describe(&data);
    }
}

#[test]
fn actual_rutoken_descriptor_endpoint_count_quirk() {
    // Cached public USB descriptors from the already-working physical token.
    // iSerialNumber is zero; no PIN, certificate or transfer payload is included.
    let raw = include_bytes!("../../../../tests/usb-fixtures/rutoken-lite-descriptors.bin");
    assert_eq!(raw.len(), 111);
    assert_eq!(raw[16], 0);
    let info=describe(raw).expect("Linux accepts this token: trust bounded endpoint descriptors, not its incorrect endpoint count");
    assert_eq!(info[1], 1);
    assert_eq!(info[2], 1);
    assert_eq!(info[3], 1);
    assert_eq!(info[7], 0);
}
