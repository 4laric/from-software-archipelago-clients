//! WHICH CONTRACT DOES THIS SEED SPEAK? -- read it, do not sniff for it (world#1463).
//!
//! The client has two location-resolution paths: the matt slot-key resolver ([`crate::key_resolver`],
//! driven by `locationIdsToKeys`) and the greenfield `locationFlags` table. Until this module it
//! chose between them by asking whether `locationIdsToKeys` happened to be present.
//!
//! A path chosen by sniffing is a path nobody validates. A seed that carries BOTH key families
//! takes the matt path silently; a bedrock seed whose `locationIdsToKeys` failed to serialize takes
//! the greenfield path silently and every check in the run resolves to nothing. Both fail the same
//! way -- in-game, hours later, as "my checks don't fire" -- because the branch was an inference,
//! and an inference has nothing to disagree with.
//!
//! So the world now DECLARES `profile` (`"greenfield"` | `"bedrock"`, contract.py, tagged BOTH and
//! required), and this module turns that declaration into a checked selection:
//!
//! * `profile` present -> the path is the declared one, and the seed must actually look like that
//!   profile. A bedrock seed with no `locationIdsToKeys`, or a greenfield seed carrying one, is a
//!   [`ProfileError`] that NAMES the key. Naming it matters: the fix is "your apworld emitted a key
//!   from the other contract", which is a one-line grep once you know which key.
//! * `profile` absent -> older seed. Fall back to exactly today's sniff and say so ONCE (a
//!   [`Selection`] with `declared: false` and a `bridge_warning`). This bridge is the whole reason the contract hash may move on a
//!   FIXPACK rather than an M bump (client AGENTS.md, V.R.M.F rule 1): a 0.6.0.3 client still plays
//!   every 0.6.0 seed, because every one of them predates `profile`.
//!
//! The foreign/required key sets are not written down here -- they are read out of the generated
//! [`crate::contract_gen::CONTRACT`] table, whose `greenfield`/`bedrock` flags come from the same
//! `contract.py` rows that decide what the world emits. There is no second list to keep in sync.

use serde_json::Value;

use crate::contract_gen::{CONTRACT, ContractKey};

/// The contract a seed speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Greenfield,
    Bedrock,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Greenfield => "greenfield",
            Profile::Bedrock => "bedrock",
        }
    }

    /// Does this profile use the matt slot-key resolver (`locationIdsToKeys`)?
    pub fn uses_key_resolver(self) -> bool {
        matches!(self, Profile::Bedrock)
    }

    /// Is `key` declared for this profile? (`BOTH` rows answer yes to both profiles.)
    fn declares(self, key: &ContractKey) -> bool {
        match self {
            Profile::Greenfield => key.greenfield,
            Profile::Bedrock => key.bedrock,
        }
    }
}

/// How the profile was decided, and what to say about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub profile: Profile,
    /// `true` when the seed declared `profile`; `false` when we fell back to the legacy sniff.
    pub declared: bool,
    /// The single line to log when this seed took the legacy bridge. `None` for a declared profile.
    pub bridge_warning: Option<String>,
}

/// A seed whose declared profile and whose actual keys disagree. Always names the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileError {
    pub message: String,
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn present(sd: &Value, name: &str) -> bool {
    !matches!(sd.get(name), None | Some(Value::Null))
}

/// Keys the generated contract declares for `other` and NOT for `profile`, that this seed emitted.
fn foreign_keys(sd: &Value, profile: Profile) -> Vec<&'static str> {
    CONTRACT
        .iter()
        .filter(|k| !profile.declares(k) && present(sd, k.name))
        .map(|k| k.name)
        .collect()
}

/// The one key each profile RESOLVES ITS LOCATIONS THROUGH. Deliberately not "every required key
/// of this profile": completeness is `contract_gen::validate`'s job and it WARNS, because a seed
/// missing an optional-in-practice key should still boot. This is the narrower question -- would
/// the branch we are about to take resolve nothing at all? -- and that is worth refusing over.
/// `path_key_is_declared_required` pins each name to a `required` row of the generated table, so a
/// rename in contract.py fails the build's tests rather than silently disarming this check.
fn path_key(profile: Profile) -> &'static str {
    match profile {
        Profile::Greenfield => "locationFlags",
        Profile::Bedrock => "locationIdsToKeys",
    }
}

/// Decide which resolution path this seed gets, from what the world DECLARED where possible.
///
/// See the module docs. `Err` is a connect-time validation failure that names the offending key.
pub fn select(sd: &Value) -> Result<Selection, ProfileError> {
    let declared = sd.get("profile").and_then(|v| v.as_str());
    let Some(declared) = declared else {
        // LEGACY BRIDGE (one release). Every 0.6.0 seed predates `profile`; refusing them here
        // would turn a fixpack into a break.
        let profile = if present(sd, "locationIdsToKeys") {
            Profile::Bedrock
        } else {
            Profile::Greenfield
        };
        return Ok(Selection {
            profile,
            declared: false,
            bridge_warning: Some(format!(
                "PROFILE: this seed declares no `profile` -- it predates the declaration \
                 (world#1463). Falling back to the legacy key-presence sniff and reading it as \
                 '{}' (locationIdsToKeys {}). Regenerate on a newer apworld to have the path \
                 validated instead of inferred.",
                profile.as_str(),
                if present(sd, "locationIdsToKeys") {
                    "present"
                } else {
                    "absent"
                },
            )),
        });
    };

    let profile = match declared {
        "greenfield" => Profile::Greenfield,
        "bedrock" => Profile::Bedrock,
        other => {
            return Err(ProfileError {
                message: format!(
                    "PROFILE: slot_data declares profile '{other}', which this client does not \
                     know (expected 'greenfield' or 'bedrock'). Refusing to guess a \
                     location-resolution path; update the client or the apworld."
                ),
            });
        }
    };

    let foreign = foreign_keys(sd, profile);
    if !foreign.is_empty() {
        return Err(ProfileError {
            message: format!(
                "PROFILE: this seed declares profile '{}' but carries {} key(s) belonging only to \
                 the other contract: {}. A key from the wrong contract is exactly what the old \
                 path sniff could not see -- the apworld emitted it, so fix the emission rather \
                 than the client.",
                profile.as_str(),
                foreign.len(),
                foreign.join(", "),
            ),
        });
    }

    let path = path_key(profile);
    if !present(sd, path) {
        return Err(ProfileError {
            message: format!(
                "PROFILE: this seed declares profile '{}' but carries no `{}`, the key that \
                 profile resolves its locations through. Every check in this run would resolve \
                 to nothing.",
                profile.as_str(),
                path,
            ),
        });
    }

    Ok(Selection {
        profile,
        declared: true,
        bridge_warning: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn declared_greenfield_selects_the_flag_table() {
        let sd = json!({ "profile": "greenfield", "locationFlags": { "1": 2 } });
        let sel = select(&sd).expect("a plain greenfield seed validates");
        assert_eq!(sel.profile, Profile::Greenfield);
        assert!(sel.declared);
        assert!(!sel.profile.uses_key_resolver());
        assert_eq!(sel.bridge_warning, None);
    }

    #[test]
    fn declared_bedrock_selects_the_key_resolver() {
        let sd = json!({
            "profile": "bedrock",
            "locationIdsToKeys": { "9110": "555000,0:0000000000:600100:" },
        });
        let sel = select(&sd).expect("a bedrock seed with its key table validates");
        assert_eq!(sel.profile, Profile::Bedrock);
        assert!(sel.profile.uses_key_resolver());
    }

    #[test]
    fn bedrock_without_the_key_table_fails_and_names_the_key() {
        let sd = json!({ "profile": "bedrock" });
        let err = select(&sd).expect_err("bedrock with no key table cannot resolve anything");
        assert!(err.message.contains("locationIdsToKeys"), "{}", err.message);
    }

    #[test]
    fn greenfield_carrying_a_bedrock_key_fails_and_names_the_foreign_key() {
        let sd = json!({
            "profile": "greenfield",
            "locationFlags": { "1": 2 },
            "locationIdsToKeys": { "9110": "555000,0:0000000000:600100:" },
        });
        let err = select(&sd).expect_err("a foreign key must not be silently ignored");
        assert!(err.message.contains("locationIdsToKeys"), "{}", err.message);
        assert!(err.message.contains("greenfield"), "{}", err.message);
    }

    #[test]
    fn a_seed_with_no_profile_takes_the_bridge_with_one_warning() {
        // The 0.6.0 seed shape: no `profile`, no matt keys. Must still play, and must say why.
        let sd = json!({ "profile": null, "locationFlags": { "1": 2 } });
        let sel = select(&sd).expect("older seeds are bridged, not refused");
        assert_eq!(sel.profile, Profile::Greenfield);
        assert!(!sel.declared);
        let warn = sel
            .bridge_warning
            .expect("the bridge announces itself exactly once");
        assert!(warn.contains("legacy key-presence sniff"), "{warn}");

        // ...and the same bridge still finds the matt path for an older foreign seed.
        let sd = json!({ "locationIdsToKeys": { "9110": "555000,0:0000000000:600100:" } });
        let sel = select(&sd).expect("older foreign seeds are bridged too");
        assert_eq!(sel.profile, Profile::Bedrock);
        assert!(!sel.declared);
        assert!(sel.bridge_warning.is_some());
    }

    #[test]
    fn an_unknown_profile_is_refused_rather_than_guessed() {
        let sd = json!({ "profile": "matt", "locationFlags": { "1": 2 } });
        let err = select(&sd).expect_err("an unknown profile has no defensible default");
        assert!(err.message.contains("matt"), "{}", err.message);
    }

    #[test]
    fn path_key_is_declared_required() {
        for profile in [Profile::Greenfield, Profile::Bedrock] {
            let name = path_key(profile);
            let key = CONTRACT
                .iter()
                .find(|k| k.name == name)
                .unwrap_or_else(|| panic!("{name} is declared in the contract"));
            assert!(
                key.required,
                "{name} must stay a required key of its profile"
            );
            assert!(
                profile.declares(key),
                "{name} must belong to {}",
                profile.as_str()
            );
        }
    }

    #[test]
    fn the_foreign_set_comes_from_the_generated_table_not_a_local_list() {
        // dungeonSweeps was retagged BEDROCK-only in the same change (world#1463); this asserts the
        // check reads the generated flags rather than a hand-kept list of key names.
        let key = CONTRACT
            .iter()
            .find(|k| k.name == "dungeonSweeps")
            .expect("declared");
        assert!(
            !key.greenfield && key.bedrock,
            "dungeonSweeps is bedrock-only"
        );
        let sd = json!({ "profile": "greenfield", "locationFlags": {}, "dungeonSweeps": {} });
        let err = select(&sd).expect_err("an empty foreign dict is still a foreign key");
        assert!(err.message.contains("dungeonSweeps"), "{}", err.message);
    }
}
