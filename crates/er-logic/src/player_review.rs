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

/// Player-facing label only. Never use this text for identity or review URLs.
/// Input may already have its sweep clause scoped to the actual seed.
pub fn pin_label(name: &str) -> String {
    let mut label = name;
    loop {
        if let Some((prefix, _)) = label.rsplit_once(" [f").filter(|(_, flag)| {
            flag.strip_suffix(']').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
        }) {
            label = prefix;
            continue;
        }
        if let Some((prefix, parts)) = label
            .rsplit_once(" (m")
            .and_then(|(prefix, map)| map.strip_suffix(')').map(|parts| (prefix, parts)))
        {
            let groups: Vec<_> = parts.split('_').collect();
            if (2..=4).contains(&groups.len())
                && groups.iter().all(|group| {
                    group.len() == 2 && group.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                label = prefix;
                continue;
            }
        }
        break;
    }
    let label = label
        .replace(
            ", may be sweep-granted by ",
            "; can also complete after defeating ",
        )
        .replace(", also granted by ", "; can also complete after defeating ");
    if let Some((region, description)) = label.split_once(" :: ") {
        format!("{}, {region}", description.replacen(" - ", " — ", 1))
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_location_cannot_change_destination_or_fragment_fields() {
        let link = url(42, r##"Sword &check=9# / "雪"##, true);
        assert_eq!(
            link,
            format!(
                "{BASE}#mode=player&check=42&p_need=&from=f6&expected_name=Sword%20%26check%3D9%23%20%2F%20%22%E9%9B%AA&p_map=1"
            )
        );
        assert_eq!(link.matches("&check=").count(), 1);
    }

    #[test]
    fn normal_review_does_not_request_map() {
        assert!(url(1, "Sword", false).ends_with("&p_map=0"));
    }
    #[test]
    fn pin_label_hides_only_known_terminal_identifiers() {
        assert_eq!(
            pin_label("Limgrave :: Flail - near Agheel Lake North [f1042377060]"),
            "Flail — near Agheel Lake North, Limgrave"
        );
        assert_eq!(pin_label("Treasure (m32_01) [f32017040]"), "Treasure");
        assert_eq!(pin_label("Treasure [friend]"), "Treasure [friend]");
        assert_eq!(pin_label("Treasure (mystery)"), "Treasure (mystery)");
        assert_eq!(
            pin_label("Treasure [f12] beside cliff"),
            "Treasure [f12] beside cliff"
        );
        assert_eq!(pin_label("Treasure (m32_1)"), "Treasure (m32_1)");
    }

    #[test]
    fn pin_label_preserves_seed_scoped_boss_information_in_plain_language() {
        assert_eq!(
            pin_label("Limgrave :: Scrap - treasure, may be sweep-granted by Troll (m32_01) [f12]"),
            "Scrap — treasure; can also complete after defeating Troll, Limgrave"
        );
    }
}
