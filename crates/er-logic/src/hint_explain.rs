//! Explanations for items that are deliberately absent from the AP pool but look hintable.

pub const SERPENT_HUNTER_REPLY: &str =
    "Serpent-Hunter is granted automatically when you enter Rykard's arena; it is not placed in the multiworld.";

/// Whether an outgoing room command is trying to hint Serpent-Hunter. AP broadcasts the raw
/// command as structured Chat before returning CommandResult, so this does not scrape rendered
/// server text and works when the command came from the web client attached to the same slot.
pub fn is_serpent_hunter_hint(message: &str) -> bool {
    let mut parts = message.split_whitespace();
    let Some(command) = parts.next() else {
        return false;
    };
    if !command.eq_ignore_ascii_case("!hint") && !command.eq_ignore_ascii_case("/hint") {
        return false;
    }
    let normalized: String = parts
        .flat_map(str::chars)
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    normalized == "serpenthunter"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_server_and_console_spellings() {
        for line in [
            "!hint Serpent-Hunter",
            "!HINT serpent hunter",
            "/hint Serpent_Hunter",
        ] {
            assert!(is_serpent_hunter_hint(line), "{line}");
        }
    }

    #[test]
    fn ignores_other_chat_and_other_hints() {
        for line in [
            "Serpent-Hunter",
            "!hint Serpent Bow",
            "!hint_location Serpent-Hunter",
            "!hint",
        ] {
            assert!(!is_serpent_hunter_hint(line), "{line}");
        }
    }
}
