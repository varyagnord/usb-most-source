use super::*;
use serde_json::{Value, json};

fn value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}
fn apply(input: &[u8], request: Value, running: bool) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    execute(input, &serde_json::to_vec(&request).unwrap(), running)
}
fn empty() -> Zeroizing<Vec<u8>> {
    apply(&[], json!({"op":"create"}), false).unwrap()
}
fn legacy() -> Value {
    json!({"kind":"legacy","host":"192.168.1.2","mode":1})
}
fn ssh(password: &str) -> Value {
    json!({"kind":"secure","profile":{"transport":"ssh","endpoint":{"host":"pc.local","port":22},
        "user":"fixture","host_key":null,"reverse_port":0,"prepare":true},
        "credentials":{"password":password,"sudo_password":"synthetic-sudo",
            "client_id":"1234567890abcdef1234567890abcdef"}})
}
fn direct() -> Value {
    let profile = json!({"transport":"direct","endpoint":{"host":"pc.local","port":3244},
        "server_pin":crate::config::encode_pin(b"fixture cert")});
    let code = PairCode {
        endpoint: serde_json::from_value(profile["endpoint"].clone()).unwrap(),
        server_pin: profile["server_pin"].as_str().unwrap().into(),
        claim: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([12; 32]),
    }
    .encode()
    .unwrap();
    json!({"kind":"secure","profile":profile,"credentials":{"pair_code":code.as_str(),
        "pair_nonce":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7;16])}})
}
fn add(input: &[u8], name: &str, connection: Value, activate: bool) -> Zeroizing<Vec<u8>> {
    apply(
        input,
        json!({"op":"add","revision":value(input)["revision"],"name":name,
        "connection":connection,"activate":activate}),
        false,
    )
    .unwrap()
}
fn request(input: &[u8], op: &str, at: usize) -> Value {
    let c = value(input);
    json!({"op":op,"revision":c["revision"],"id":c["computers"][at]["id"]})
}
fn active(input: &[u8]) -> Value {
    value(&apply(input, json!({"op":"active"}), false).unwrap())
}
fn deny(input: &[u8], request: Value, running: bool, code: &str) {
    let before = input.to_vec();
    match apply(input, request, running) {
        Err(actual) => assert_eq!(actual, code),
        Ok(_) => panic!("unexpected successful mutation"),
    }
    assert_eq!(
        input, before,
        "a rejected transaction must not change caller-owned storage"
    );
}

#[test]
fn separate_records_do_not_share_credentials_even_for_same_address_and_name() {
    let a = add(&empty(), "Работа", ssh("password-one"), true);
    let b = add(&a, "Работа", ssh("password-two"), false);
    let ids = value(&b);
    assert_ne!(ids["computers"][0]["id"], ids["computers"][1]["id"]);
    assert_eq!(
        active(&b)["connection"]["credentials"]["password"],
        "password-one"
    );
    let b = apply(&b, request(&b, "activate", 1), false).unwrap();
    assert_eq!(
        active(&b)["connection"]["credentials"]["password"],
        "password-two"
    );
    let edited = value(&apply(&b, request(&b, "edit", 0), false).unwrap());
    assert_eq!(
        edited["connection"]["credentials"]["password"],
        "password-one"
    );
    let public = apply(&b, json!({"op":"view"}), false).unwrap();
    let public = std::str::from_utf8(&public).unwrap();
    for secret in [
        "password-one",
        "password-two",
        "synthetic-sudo",
        "client_id",
        "credentials",
        "pair_code",
        "token",
    ] {
        assert!(!public.contains(secret), "secret leaked into public DTO");
    }
    assert!(public.contains("Работа"));
}

#[test]
fn migration_requires_exact_legacy_binding_and_never_reimports_into_v2() {
    let connection = ssh("legacy-password");
    let profile = connection["profile"].to_string();
    let mut migrate = json!({"op":"migrate","name":"Прежний компьютер",
        "expected_profile":profile,"old":{"profile":profile,"credentials":connection["credentials"]}});
    // The old Android form writes both fields; SSH chooses the key if present.
    migrate["old"]["credentials"]["private_key"] = json!("synthetic-private-key");
    let c = apply(&[], migrate.clone(), false).unwrap();
    assert_eq!(value(&c)["revision"], 1);
    assert_eq!(
        active(&c)["connection"]["credentials"]["password"],
        "legacy-password"
    );
    assert_eq!(
        active(&c)["connection"]["credentials"]["private_key"],
        "synthetic-private-key"
    );
    deny(&c, migrate.clone(), false, "catalog_exists");
    deny(&empty(), migrate.clone(), false, "catalog_exists");
    migrate["expected_profile"] = json!(profile.replace("pc.local", "other.local"));
    deny(&[], migrate.clone(), false, "profile_mismatch");
    migrate["expected_profile"] = json!(format!(" {profile}"));
    deny(&[], migrate, false, "profile_mismatch");
}

#[test]
fn rename_keeps_live_lease_but_switch_away_and_back_rejects_old_session() {
    let c = add(&empty(), "A", ssh("fixture"), true);
    let c = add(&c, "B", legacy(), false);
    let lease = active(&c)["lease"].clone();
    let mut rename = request(&c, "rename", 0);
    rename["name"] = json!("Новое имя");
    let c = apply(&c, rename, true).unwrap();
    assert_eq!(active(&c)["lease"], lease);
    let trust = json!({"op":"trust","lease":lease,"pin":crate::config::encode_pin(b"key")});
    assert!(apply(&c, trust.clone(), true).is_ok());
    let c = apply(&c, request(&c, "activate", 1), false).unwrap();
    let c = apply(&c, request(&c, "activate", 0), false).unwrap();
    deny(&c, trust, true, "stale_connection");
}

#[test]
fn every_new_session_invalidates_previous_lease_on_the_same_computer() {
    let c = add(&empty(), "A", ssh("fixture"), true);
    let old = active(&c)["lease"].clone();
    let begin = json!({"op":"begin","revision":value(&c)["revision"]});
    deny(&c, begin.clone(), true, "connection_running");
    let c = apply(&c, begin, false).unwrap();
    deny(
        &c,
        json!({"op":"trust","lease":old,"pin":crate::config::encode_pin(b"key")}),
        true,
        "stale_connection",
    );
    assert_ne!(active(&c)["lease"], old);
    deny(
        &empty(),
        json!({"op":"begin","revision":0}),
        false,
        "computer_not_selected",
    );
}

#[test]
fn active_replacement_forget_and_switch_require_stopped_bridge() {
    let c = add(&empty(), "A", ssh("fixture"), true);
    let c = add(&c, "B", legacy(), false);
    deny(&c, request(&c, "forget", 0), true, "connection_running");
    deny(&c, request(&c, "activate", 1), true, "connection_running");
    let mut replace = request(&c, "replace", 0);
    replace["name"] = json!("A");
    replace["connection"] = ssh("new-password");
    deny(&c, replace.clone(), true, "connection_running");
    deny(
        &c,
        json!({"op":"add","revision":2,"name":"C","connection":legacy(),"activate":true}),
        true,
        "connection_running",
    );
    assert!(apply(&c, request(&c, "forget", 1), true).is_ok());
    // An inactive computer can be edited, but its old credential never survives replacement.
    replace["id"] = value(&c)["computers"][1]["id"].clone();
    let next = apply(&c, replace, true).unwrap();
    assert_eq!(value(&next)["computers"][1]["connection_generation"], 2);
    assert_eq!(
        active(&next)["connection"]["credentials"]["password"],
        "fixture"
    );
}

#[test]
fn stale_ui_and_missing_identity_never_overwrite_newer_changes() {
    let c = add(&empty(), "A", legacy(), true);
    let stale = request(&c, "forget", 0);
    let next = add(&c, "B", legacy(), false);
    deny(&next, stale, false, "catalog_changed");
    let mut missing = request(&next, "edit", 0);
    missing["id"] = json!("0".repeat(32));
    deny(&next, missing, false, "computer_missing");
    deny(&next, request(&c, "edit", 0), false, "catalog_changed");
}

#[test]
fn forget_clears_active_without_selecting_another_and_empty_is_not_uninitialized() {
    let c = add(&empty(), "A", ssh("must-disappear"), true);
    let c = add(&c, "B", legacy(), false);
    let c = apply(&c, request(&c, "forget", 0), false).unwrap();
    assert!(value(&c)["active_id"].is_null());
    assert!(!std::str::from_utf8(&c).unwrap().contains("must-disappear"));
    deny(&c, json!({"op":"active"}), false, "computer_not_selected");
    let c = apply(&c, request(&c, "forget", 0), false).unwrap();
    assert_eq!(value(&c)["computers"], json!([]));
    assert!(apply(&c, json!({"op":"view"}), false).is_ok());
    deny(&c, json!({"op":"create"}), false, "catalog_exists");
    deny(b"corrupt", json!({"op":"create"}), false, "catalog_exists");
    deny(b"corrupt", json!({"op":"view"}), false, "invalid_catalog");
}

#[test]
fn trust_pin_is_immutable_and_duplicate_ack_requires_current_lease() {
    let c = add(&empty(), "A", ssh("fixture"), true);
    let pin = crate::config::encode_pin(b"host-key");
    let first = json!({"op":"trust","lease":active(&c)["lease"],"pin":pin});
    deny(&c, first.clone(), false, "stale_connection");
    let c = apply(&c, first.clone(), true).unwrap();
    deny(&c, first, true, "stale_connection");
    let repeated = json!({"op":"trust","lease":active(&c)["lease"],"pin":pin});
    let same = apply(&c, repeated, true).unwrap();
    assert_eq!(*c, *same);
    deny(
        &c,
        json!({"op":"trust","lease":active(&c)["lease"],"pin":crate::config::encode_pin(b"other-key")}),
        true,
        "identity_changed",
    );
}

#[test]
fn pairing_replaces_one_time_claim_atomically_and_cannot_rotate_existing_token() {
    let c = add(&empty(), "Pair", direct(), true);
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([22; 32]);
    let paired = json!({"op":"paired","lease":active(&c)["lease"],"token":key});
    let c = apply(&c, paired.clone(), true).unwrap();
    let s = active(&c)["connection"]["credentials"].clone();
    assert_eq!(s["token"], key);
    assert!(s["pair_code"].is_null() && s["pair_nonce"].is_null());
    deny(&c, paired, true, "stale_connection");
    let repeated = json!({"op":"paired","lease":active(&c)["lease"],"token":key});
    assert_eq!(*c, *apply(&c, repeated, true).unwrap());
    deny(
        &c,
        json!({"op":"paired","lease":active(&c)["lease"],"token":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([23;32])}),
        true,
        "identity_changed",
    );
    let public = apply(&c, json!({"op":"view"}), false).unwrap();
    assert!(!std::str::from_utf8(&public).unwrap().contains(&key));
    deny(
        &c,
        json!({"op":"trust","lease":active(&c)["lease"],"pin":crate::config::encode_pin(b"key")}),
        true,
        "wrong_transport",
    );
}

#[test]
fn untrusted_input_has_strict_schema_unique_ids_and_no_dangling_active() {
    let c = add(&empty(), "A", legacy(), true);
    for bad in [
        json!({"schema":1,"revision":0,"active_id":null,"activation_epoch":0,"computers":[]}),
        {
            let mut v = value(&c);
            let duplicate = v["computers"][0].clone();
            v["computers"].as_array_mut().unwrap().push(duplicate);
            v
        },
        {
            let mut v = value(&c);
            v["active_id"] = json!("f".repeat(32));
            v
        },
        {
            let mut v = value(&c);
            v["activation_epoch"] = json!(0);
            v
        },
        {
            let mut v = value(&c);
            v["computers"][0]["connection_generation"] = json!(0);
            v
        },
        {
            let mut v = value(&c);
            v["revision"] = json!(u64::MAX);
            v
        },
        {
            let mut v = value(&c);
            v["extra"] = json!("secret");
            v
        },
        {
            let mut v = value(&c);
            v["computers"][0]["connection"]["credentials"] = json!({});
            v
        },
    ] {
        assert!(
            apply(
                &serde_json::to_vec(&bad).unwrap(),
                json!({"op":"view"}),
                false
            )
            .is_err()
        );
    }
    deny(
        &c,
        json!({"op":"view","secret":"not-echoed"}),
        false,
        "invalid_catalog_request",
    );
    assert_eq!(
        execute(&c, br#"{"op":"view","op":"create"}"#, false).err(),
        Some("invalid_catalog_request")
    );
    assert_eq!(
        execute(&vec![b' '; MAX_CATALOG + 1], b"{}", false).err(),
        Some("catalog_too_large")
    );
    assert_eq!(
        execute(&c, &vec![b' '; MAX_REQUEST + 1], false).err(),
        Some("catalog_request_too_large")
    );
    assert_eq!(
        execute(&c, &[0xff], false).err(),
        Some("invalid_catalog_request")
    );
    assert_eq!(
        execute(
            &c,
            format!("{}{}", "[".repeat(150), "]".repeat(150)).as_bytes(),
            false
        )
        .err(),
        Some("invalid_catalog_request")
    );
}

#[test]
fn names_capacity_and_counter_exhaustion_fail_without_partial_commit() {
    let c = add(&empty(), "A", legacy(), true);
    for name in [
        "".into(),
        " ".into(),
        " x".into(),
        "x ".into(),
        "a\nb".into(),
        "a\u{202e}b".into(),
        "a\u{2067}b".into(),
        "a\u{2028}b".into(),
        "a\0b".into(),
        "x".repeat(65),
        "😀".repeat(65),
    ] {
        let mut rename = request(&c, "rename", 0);
        rename["name"] = json!(name);
        deny(&c, rename, false, "invalid_computer_name");
    }
    let c = add(&c, &"😀".repeat(64), legacy(), false);
    let mut full = c;
    for _ in 2..MAX_COMPUTERS {
        full = add(&full, "Same name", legacy(), false);
    }
    deny(
        &full,
        json!({"op":"add","revision":32,"name":"Overflow","connection":legacy(),"activate":false}),
        false,
        "computer_limit",
    );
    for field in ["revision", "activation_epoch"] {
        let mut exhausted = value(&full);
        exhausted[field] = json!(MAX_COUNTER);
        let exhausted = serde_json::to_vec(&exhausted).unwrap();
        deny(
            &exhausted,
            request(&exhausted, "forget", 0),
            false,
            "counter_exhausted",
        );
    }
    let mut exhausted = value(&full);
    exhausted["computers"][0]["connection_generation"] = json!(MAX_COUNTER);
    let exhausted = serde_json::to_vec(&exhausted).unwrap();
    let mut replace = request(&exhausted, "replace", 0);
    replace["name"] = json!("new");
    replace["connection"] = legacy();
    deny(&exhausted, replace, false, "counter_exhausted");
}

#[test]
fn credentials_validate_transport_bounds_and_pairing_identity_before_persistence() {
    let c = empty();
    let good = ssh("fixture");
    for bad in [
        {
            let mut v = good.clone();
            v["credentials"]["token"] = json!("x");
            v
        },
        {
            let mut v = good.clone();
            v["credentials"]["password"] = json!("x".repeat(4097));
            v
        },
        {
            let mut v = good.clone();
            v["credentials"]["sudo_password"] = json!("line\nother");
            v
        },
        {
            let mut v = good.clone();
            v["credentials"]["password"] = Value::Null;
            v
        },
        {
            let mut v = good.clone();
            v["credentials"]["client_id"] = Value::Null;
            v
        },
        {
            let mut v = direct();
            v["profile"]["endpoint"]["host"] = json!("different.local");
            v
        },
        {
            let mut v = direct();
            v["credentials"]["pair_nonce"] =
                json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0; 16]));
            v
        },
        {
            let mut v = direct();
            v["credentials"]["password"] = json!("foreign");
            v
        },
        {
            let mut v = direct();
            v["credentials"]["token"] =
                json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([1; 32]));
            v
        },
        json!({"kind":"legacy","host":"8.8.8.8","mode":1}),
        json!({"kind":"legacy","host":"010.0.0.1","mode":1}),
    ] {
        assert!(
            apply(
                &c,
                json!({"op":"add","revision":0,"name":"A","connection":bad,"activate":true}),
                false
            )
            .is_err()
        );
    }
    let c = add(&c, "A", good, true);
    let mut replacement = request(&c, "replace", 0);
    replacement["name"] = json!("A");
    replacement["connection"] = legacy();
    let c = apply(&c, replacement, false).unwrap();
    let text = std::str::from_utf8(&c).unwrap();
    assert!(!text.contains("fixture") && !text.contains("credentials"));
}

#[test]
fn usb_rules_are_per_computer_revision_bound_and_retired_with_destination() {
    let a = add(&empty(), "Same", legacy(), true);
    let a = add(&a, "Same", legacy(), false);
    let identity =
        serde_json::to_value(Identity::recognize(1, 2, Some("fixture"), "hid").unwrap()).unwrap();
    let mut set = request(&a, "set_usb_rule", 0);
    set["rule"] = json!({"identity":identity,"name":"Мышь","automatic":true,"storage":false});
    let b = apply(&a, set.clone(), true).unwrap();
    assert_eq!(
        value(&b)["computers"][0]["usb_rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        value(&b)["computers"][1]["usb_rules"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    deny(&b, set, true, "catalog_changed");
    assert_eq!(
        active(&a)["lease"],
        active(&b)["lease"],
        "future rule editing does not retire a session"
    );
    let mut rename = request(&b, "rename", 0);
    rename["name"] = json!("Work");
    let c = apply(&b, rename, true).unwrap();
    assert_eq!(
        value(&b)["computers"][0]["usb_rules"],
        value(&c)["computers"][0]["usb_rules"]
    );
    let mut replace = request(&c, "replace", 0);
    replace["name"] = json!("Work");
    replace["connection"] = legacy();
    let d = apply(&c, replace, false).unwrap();
    assert_eq!(value(&d)["computers"][0]["usb_epoch"], 2);
    assert_eq!(
        value(&d)["computers"][0]["usb_rules"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let forgotten = apply(&c, request(&c, "forget", 0), false).unwrap();
    assert!(!std::str::from_utf8(&forgotten).unwrap().contains("Мышь"));
    let mut forget_rule = request(&c, "forget_usb_rule", 0);
    forget_rule["identity"] = identity;
    let forgotten = apply(&c, forget_rule, false).unwrap();
    assert_eq!(
        value(&forgotten)["computers"][0]["usb_rules"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn older_catalog_defaults_to_no_usb_rules_and_queries_never_mutate_or_leak_serial() {
    let a = add(&empty(), "Old", legacy(), true);
    let mut old = value(&a);
    old["computers"][0]
        .as_object_mut()
        .unwrap()
        .remove("usb_rules");
    old["computers"][0]
        .as_object_mut()
        .unwrap()
        .remove("usb_epoch");
    let restored = apply(old.to_string().as_bytes(), json!({"op":"view"}), false).unwrap();
    assert_eq!(value(&restored)["computers"][0]["usb_epoch"], 1);
    assert_eq!(value(&restored)["computers"][0]["usb_rules"], json!([]));
    let query = json!({"op":"usb_identity","vendor":1,"product":2,"serial":"fixture-private-serial","layout":"hid"});
    let identity = apply(&[], query.clone(), false).unwrap();
    assert!(
        !std::str::from_utf8(&identity)
            .unwrap()
            .contains("fixture-private-serial")
    );
    deny(&a, query.clone(), false, "invalid_catalog_request");
    deny(&[], query, true, "invalid_catalog_request");
    assert_eq!(
        value(
            &apply(
                &[],
                json!({"op":"usb_evaluate","rules":[],"devices":[]}),
                false
            )
            .unwrap()
        ),
        json!([])
    );
}

#[test]
fn first_trust_preserves_explicit_usb_rules_and_names_validate_without_catalog() {
    let a = add(&empty(), "Work", ssh("fixture"), true);
    let identity = Identity::recognize(1, 2, Some("fixture"), "hid").unwrap();
    let mut set = request(&a, "set_usb_rule", 0);
    set["rule"] = json!({"identity":identity,"name":"USB","automatic":true,"storage":false});
    let b = apply(&a, set, false).unwrap();
    let c = apply(&b,json!({"op":"trust","lease":active(&b)["lease"],"pin":crate::config::encode_pin(b"fixture key")}),true).unwrap();
    assert_eq!(
        value(&b)["computers"][0]["usb_rules"],
        value(&c)["computers"][0]["usb_rules"]
    );
    assert_eq!(
        value(&b)["computers"][0]["usb_epoch"],
        value(&c)["computers"][0]["usb_epoch"]
    );
    for (name, valid) in [
        ("Мышь", true),
        ("", true),
        ("bad\nname", false),
        (" x", false),
    ] {
        assert_eq!(
            value(&apply(&[], json!({"op":"usb_name","name":name}), false).unwrap()),
            json!(valid)
        );
    }
}
