use super::*;

fn identity(serial: &str) -> Identity {
    Identity::recognize(0x046d, 0xc52b, Some(serial), "fixture-hid-layout").unwrap()
}
fn rule() -> Rule {
    Rule {
        identity: identity("fixture-a"),
        name: "Мышь".into(),
        automatic: true,
        storage: false,
    }
}
fn device(id: u32, serial: &str) -> Device {
    Device {
        id,
        generation: 100 + id as i64,
        vendor: 0x046d,
        product: 0xc52b,
        identity: Some(identity(serial)),
        storage: false,
        supported: true,
        permission: true,
    }
}

#[test]
fn deterministic_recognition_without_plain_serial_or_bus_address() {
    let a = identity("fixture-a");
    assert!(a == identity("fixture-a"));
    let encoded = serde_json::to_string(&a).unwrap();
    assert!(!encoded.contains("fixture"));
    assert_eq!(a.serial_hash.len(), 64);
    assert!(a != identity("fixture-b"));
    assert!(a != Identity::recognize(1, 2, Some("fixture-a"), "fixture-hid-layout").unwrap());
    assert!(a != Identity::recognize(0x046d, 0xc52b, Some("fixture-a"), "other").unwrap());
}

#[test]
fn unreadable_empty_control_and_oversize_serial_never_identify() {
    for serial in [None, Some(""), Some("   "), Some("bad\nserial"), Some("\0")] {
        assert!(Identity::recognize(1, 2, serial, "layout").is_none());
    }
    assert!(Identity::recognize(1, 2, Some(&"x".repeat(1025)), "layout").is_none());
    assert!(Identity::recognize(1, 2, Some("ok"), "").is_none());
    assert!(Identity::recognize(1, 2, Some("ok"), &"x".repeat(8193)).is_none());
}

#[test]
fn hotplug_different_hub_port_preserves_rule_but_returns_new_generation() {
    let rules = [rule()];
    for id in [1, 2005, 6009] {
        let devices = [device(id, "fixture-a")];
        let result = evaluate(&rules, &devices).unwrap();
        assert_eq!(result[0].id, id);
        assert_eq!(result[0].generation, 100 + id as i64);
        assert_eq!(result[0].name, "Мышь");
        assert!(result[0].automatic);
    }
}

#[test]
fn no_rule_and_different_serial_or_layout_are_manual() {
    let rules = [rule()];
    let mut changed = device(1, "fixture-a");
    changed.identity.as_mut().unwrap().layout_hash = "a".repeat(64);
    for devices in [vec![device(1, "fixture-b")], vec![changed]] {
        let result = evaluate(&rules, &devices).unwrap();
        assert!(!result[0].automatic);
        assert_eq!(result[0].name, "");
    }
    assert!(!evaluate(&[], &[device(1, "fixture-a")]).unwrap()[0].automatic);
}

#[test]
fn duplicate_serial_even_with_different_layout_is_ambiguous_in_any_order() {
    let rules = [rule()];
    for different_layout in [false, true] {
        let mut devices = vec![device(1, "fixture-a"), device(2, "fixture-a")];
        if different_layout {
            devices[1].identity.as_mut().unwrap().layout_hash = "b".repeat(64);
        }
        for _ in 0..2 {
            for result in evaluate(&rules, &devices).unwrap() {
                assert!(!result.automatic);
                assert_eq!(result.name, "");
                assert_eq!(result.reason, "ambiguous");
            }
            devices.reverse();
        }
    }
}

#[test]
fn unknown_identical_model_blocks_known_device_but_distinct_serial_does_not() {
    let rules = [rule()];
    let mut devices = vec![device(1, "fixture-a"), device(2, "fixture-b")];
    assert!(evaluate(&rules, &devices).unwrap()[0].automatic);
    devices[1].permission = false;
    assert_eq!(evaluate(&rules, &devices).unwrap()[0].reason, "ambiguous");
    devices[1].permission = true;
    devices[1].identity = None;
    devices[1].permission = false;
    assert_eq!(evaluate(&rules, &devices).unwrap()[0].reason, "ambiguous");
    devices[1].product = 99;
    assert!(evaluate(&rules, &devices).unwrap()[0].automatic);
}

#[test]
fn storage_permission_and_capability_cannot_be_overridden_by_rule() {
    let rules = [rule()];
    for expected in ["storage", "permission", "unsupported", "no_identity"] {
        let mut device = device(1, "fixture-a");
        match expected {
            "storage" => device.storage = true,
            "permission" => device.permission = false,
            "unsupported" => device.supported = false,
            _ => device.identity = None,
        }
        let devices = [device];
        let result = evaluate(&rules, &devices).unwrap();
        assert!(!result[0].automatic);
        assert_eq!(result[0].reason, expected);
    }
    let mut disk = rule();
    disk.storage = true;
    assert_eq!(disk.validate(), Err("storage_requires_consent"));
    disk.automatic = false;
    let devices = [device(1, "fixture-a")];
    let rules = [disk];
    let result = evaluate(&rules, &devices).unwrap();
    assert_eq!(result[0].name, "Мышь");
    assert!(!result[0].automatic);
}

#[test]
fn bounded_validated_inventory_and_unique_rules() {
    assert!(evaluate(&[rule()], &[device(1, "fixture-a"), device(1, "fixture-b")]).is_err());
    assert!(
        evaluate(
            &[rule()],
            &(0..129).map(|i| device(i, "fixture-a")).collect::<Vec<_>>()
        )
        .is_err()
    );
    assert_eq!(validate(&[rule(), rule()]), Err("duplicate_usb_rule"));
    assert_eq!(validate(&vec![rule(); 65]), Err("usb_rule_limit"));
    let mut wrong = device(1, "fixture-a");
    wrong.vendor = 1;
    assert!(evaluate(&[rule()], &[wrong]).is_err());
    for bad in [" ", "x\ny", "\u{202e}spoof", &"x".repeat(65)] {
        let mut rule = rule();
        rule.name = bad.into();
        assert_eq!(rule.validate(), Err("invalid_usb_name"));
    }
    for bad in ["", "g", &"A".repeat(64), &"a".repeat(65)] {
        let mut rule = rule();
        rule.identity.serial_hash = bad.into();
        assert_eq!(rule.validate(), Err("invalid_usb_identity"));
    }
}
