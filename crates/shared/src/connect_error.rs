//! Why a connect attempt died, in words that do not point triage at the thing it already ruled
//! out (client#181).
//!
//! # The motivating case
//!
//! 08-12, player `Doug`, room `archipelago.gg:59042`. He got this and nothing else:
//!
//! ```text
//! Connection refused. Make sure the server session is running and the URL is up-to-date.
//! ```
//!
//! Four rounds of triage went on the URL, the `apconfig.json`, the room page and the port. All
//! correct. Then the stock AP text client connected from his machine to the same host and port --
//! so the network path was fine, the config was never in play, and **every one of those four rounds
//! was spent on a hypothesis this code path had already excluded.**
//!
//! # What the old sentence got wrong, twice
//!
//! * **It collapsed two opposite diagnoses into one.** A rejected SYN (nothing listening: wrong
//!   port, paused room) and a dropped SYN (firewall, AV, per-app VPN split tunnel) want opposite
//!   fixes, and the message named neither. The only signal left was how long the player waited
//!   before the red line, which nothing asks for and nothing logs.
//! * 🛑 **It advertised the one thing it had already excluded.** "Make sure the URL is up-to-date"
//!   points at config -- but a bad slot, bad password, wrong game, seed mismatch and version
//!   mismatch all take the `Disconnected:` / `Connection failed:` branches instead, and so does a
//!   DNS failure (Windows name resolution does not surface as `ConnectionRefused`/`TimedOut`). By
//!   the time this arm runs, **the name resolved and the socket still never opened.** Naming the
//!   URL here is not merely unhelpful; it is provably wrong, and it cost four rounds.
//!
//! # ⚠️ Cross-game wording
//!
//! `shared` backs the Elden Ring, DS3 and Sekiro clients, so the firewall line says "the game's
//! executable" rather than naming `eldenring.exe` as #181 does. The player can read their own
//! title bar; a client that confidently names the wrong .exe is the same defect this module is
//! about, one layer down.

use std::io::ErrorKind;

/// A socket that never opened, split by what actually happened to the packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectFailure {
    /// The SYN was REJECTED. Something answered and said no: nothing is listening on that port.
    Refused,
    /// The SYN went nowhere. Nothing answered at all.
    TimedOut,
}

/// Classify a socket-level connect failure. `None` = not this shape, and the caller must fall
/// through to its other branches rather than guessing.
///
/// 🛑 ONLY THESE TWO KINDS. Every other failure mode -- bad slot, bad password, wrong game, seed
/// or version mismatch, DNS -- reaches the player through a different branch with the server's own
/// text, and widening this match would take those over and describe them wrongly.
pub fn classify(kind: ErrorKind) -> Option<ConnectFailure> {
    match kind {
        ErrorKind::ConnectionRefused => Some(ConnectFailure::Refused),
        ErrorKind::TimedOut => Some(ConnectFailure::TimedOut),
        _ => None,
    }
}

impl ConnectFailure {
    /// The red headline.
    pub fn headline(&self) -> &'static str {
        match self {
            ConnectFailure::Refused => "Connection refused. ",
            // Deliberately NOT "Connection refused": that word means a server said no, and this is
            // the case where nothing said anything. A player who reports the wrong word sends
            // triage down the wrong branch before anyone reads a log.
            ConnectFailure::TimedOut => "Connection timed out. ",
        }
    }

    /// What to actually check. ASCII only -- this reaches the player through the in-game overlay.
    pub fn advice(&self) -> &'static str {
        match self {
            // The room-paused case is worth naming outright: archipelago.gg parks a room after
            // ~2h idle and a page refresh resumes it, which is a fix the player can apply in ten
            // seconds and would otherwise never guess.
            ConnectFailure::Refused => {
                "The host answered but nothing is listening on that port. Check the port number, \
                 and if the room is on archipelago.gg open its page -- a room parked after a few \
                 hours idle resumes when the page is loaded. The address itself resolved, so the \
                 host name is not the problem."
            }
            ConnectFailure::TimedOut => {
                "The packets went nowhere -- nothing answered on that port. This is almost always \
                 something on this machine blocking the game: an outbound firewall rule on the \
                 game's executable, antivirus network protection, or a VPN with split tunnelling. \
                 The address resolved and the slot details were never sent, so the URL, slot name \
                 and password are NOT the cause. Worth trying: the standalone Archipelago text \
                 client from this same machine -- if that connects and this does not, it is the \
                 game process being blocked."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⭐ THE RED-FIRST ASSERTION. One sentence for both kinds is the defect; the two must not be
    /// able to drift back into agreement.
    #[test]
    fn the_two_failures_do_not_share_a_sentence() {
        let refused = ConnectFailure::Refused;
        let timed_out = ConnectFailure::TimedOut;
        assert_ne!(
            refused.headline(),
            timed_out.headline(),
            "a rejected SYN and a dropped SYN are opposite diagnoses"
        );
        assert_ne!(refused.advice(), timed_out.advice());
    }

    /// 🛑 THE KEEPER. By the time this arm runs the name has resolved and the slot details were
    /// never sent, so pointing the player at the URL is provably wrong -- that is the sentence
    /// that cost Doug four rounds of triage.
    #[test]
    fn neither_message_sends_the_player_back_to_the_url() {
        for f in [ConnectFailure::Refused, ConnectFailure::TimedOut] {
            let text = f.advice().to_ascii_lowercase();
            assert!(
                !text.contains("make sure the server session is running and the url"),
                "the old sentence is back: {text}"
            );
            // It may MENTION the URL, but only to exclude it.
            if text.contains("url") || text.contains("host name") {
                assert!(
                    text.contains("not the cause")
                        || text.contains("not the problem")
                        || text.contains("resolved"),
                    "the address may only be named in order to rule it out: {text}"
                );
            }
        }
    }

    /// The timeout message names the causes worth checking, and the refused one names the paused
    /// room. These are the two fixes a player can actually apply.
    #[test]
    fn each_message_names_something_actionable() {
        let t = ConnectFailure::TimedOut.advice().to_ascii_lowercase();
        for want in ["firewall", "antivirus", "vpn"] {
            assert!(t.contains(want), "timeout advice must name {want}: {t}");
        }
        let r = ConnectFailure::Refused.advice().to_ascii_lowercase();
        assert!(r.contains("port"), "{r}");
        assert!(r.contains("archipelago.gg"), "the paused-room fix: {r}");
    }

    /// 🛑 ONLY THESE TWO KINDS. Everything else reaches the player through a branch carrying the
    /// server's own text; widening this match would take those over and describe them wrongly.
    #[test]
    fn only_the_socket_level_kinds_classify() {
        assert_eq!(
            classify(ErrorKind::ConnectionRefused),
            Some(ConnectFailure::Refused)
        );
        assert_eq!(
            classify(ErrorKind::TimedOut),
            Some(ConnectFailure::TimedOut)
        );
        for other in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::NotConnected,
            ErrorKind::PermissionDenied,
            ErrorKind::Other,
        ] {
            assert_eq!(
                classify(other),
                None,
                "{other:?} is not this arm's business"
            );
        }
    }

    /// In-game strings are ASCII-only (repo rule).
    #[test]
    fn the_messages_are_ascii() {
        for f in [ConnectFailure::Refused, ConnectFailure::TimedOut] {
            assert!(f.headline().is_ascii(), "{}", f.headline());
            assert!(f.advice().is_ascii(), "{}", f.advice());
        }
    }
}
