//! Explicit player-review links. No network requests, game writes, or seed secrets.
pub const BASE: &str = "https://peliarch.ca/er/beta/review.html";

fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(out, "%{byte:02X}").expect("writing to String");
        }
    }
    out
}

/// The original server location name protects against catalog ID reuse. It is context,
/// never a player observation. Deliberately excludes slot, server, password and seed.
pub fn url(id: u64, name: &str, map: bool) -> String {
    format!(
        "{BASE}#mode=player&check={id}&p_need=&from=f6&expected_name={}&p_map={}",
        encode(name),
        u8::from(map)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_location_cannot_change_destination_or_fragment_fields() {
        let link = url(42, r##"Sword &check=9# / "雪"##, true);
        assert_eq!(link, format!("{BASE}#mode=player&check=42&p_need=&from=f6&expected_name=Sword%20%26check%3D9%23%20%2F%20%22%E9%9B%AA&p_map=1"));
        assert_eq!(link.matches("&check=").count(), 1);
    }

    #[test]
    fn normal_review_does_not_request_map() {
        assert!(url(1, "Sword", false).ends_with("&p_map=0"));
    }
}
