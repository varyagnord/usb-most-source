//! Public, build-owned store link; never derived from a pairing code or peer.
pub fn parse(text: &str) -> Result<Option<String>, &'static str> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "invalid listing JSON")?;
    let object = value.as_object().ok_or("listing must be an object")?;
    if object.len() != 1 {
        return Err("listing must contain only url");
    }
    match object.get("url") {
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(url)) => {
            let package = url
                .strip_prefix("https://www.rustore.ru/catalog/app/")
                .ok_or("expected canonical HTTPS RuStore app link")?;
            if package.is_empty()
                || package.len() > 255
                || !package
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
                || !package.bytes().next().unwrap().is_ascii_alphanumeric()
                || !package.bytes().last().unwrap().is_ascii_alphanumeric()
            {
                return Err("invalid RuStore app identifier");
            }
            Ok(Some(url.clone()))
        }
        _ => Err("url must be a string or null"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listing_is_optional_but_never_an_arbitrary_navigation_target() {
        assert!(parse(include_str!("rustore.json")).is_ok());
        assert_eq!(parse(r#"{"url":null}"#).unwrap(), None);
        for id in ["ru.example.usbmost", "12345"] {
            let url = format!("https://www.rustore.ru/catalog/app/{id}");
            let text = serde_json::json!({"url":url}).to_string();
            assert_eq!(parse(&text).unwrap().as_deref(), Some(url.as_str()));
        }
        for url in [
            "http://www.rustore.ru/catalog/app/x",
            "https://www.rustore.ru.evil/catalog/app/x",
            "https://www.rustore.ru@evil/catalog/app/x",
            "javascript:alert(1)",
            "https://www.rustore.ru/catalog/app/",
            "https://www.rustore.ru/catalog/app/../x",
            "https://www.rustore.ru/catalog/app/x?referrer=secret",
            "https://www.rustore.ru/catalog/app/x#secret",
            "https://www.rustore.ru/catalog/app/%2f",
            "https://www.rustore.ru/catalog/app/x\n",
        ] {
            assert!(
                parse(&serde_json::json!({"url":url}).to_string()).is_err(),
                "{url}"
            );
        }
        for text in ["{}", "null", r#"{"url":7}"#, r#"{"url":null,"extra":true}"#] {
            assert!(parse(text).is_err());
        }
    }
}
