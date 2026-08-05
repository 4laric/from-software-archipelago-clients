use std::{fmt, sync::Arc};

use serde::de::DeserializeOwned;

use crate::protocol::{NetworkItem, NetworkItemFlags};
use crate::{Client, Error, Game, Item, Location, Player};

/// An item associated with a particular location in particular player's world.
#[derive(Clone)]
pub struct LocatedItem {
    item: Item,
    location: Location,
    sender: Arc<Player>,
    receiver: Arc<Player>,
    flags: NetworkItemFlags,
}

impl LocatedItem {
    /// Creates a fully-hydrated [LocatedItem] from a [NetworkItem].
    ///
    /// Because a [NetworkItem] alone doesn't provide full context on who the
    /// sender or receiver is, this requires them to be passed in explicitly.
    pub(crate) fn hydrate<S: DeserializeOwned>(
        network: NetworkItem,
        sender: Arc<Player>,
        receiver: Arc<Player>,
        client: &Client<S>,
    ) -> Result<LocatedItem, Error> {
        let sender_game = client.game_or_err(sender.game())?;
        let receiver_game = client.game_or_err(receiver.game())?;
        LocatedItem::hydrate_with_games(network, sender, receiver, sender_game, receiver_game)
    }

    /// Creates a fully-hydrated [LocatedItem] from an already-loaded
    /// [sender_game] and [receiver_game].
    pub(crate) fn hydrate_with_games(
        network: NetworkItem,
        sender: Arc<Player>,
        receiver: Arc<Player>,
        sender_game: &Game,
        receiver_game: &Game,
    ) -> Result<LocatedItem, Error> {
        debug_assert!(network.player == sender.slot() || network.player == receiver.slot());
        debug_assert!(sender.game() == sender_game.name());
        debug_assert!(receiver.game() == receiver_game.name());
        //
        // NEITHER lookup may fail the hydration. `Client::handle_message` hydrates a whole
        // `ReceivedItems` batch through one `collect::<Result<Vec<_>, _>>`, which short-circuits,
        // so a single unresolvable id used to discard every item in the batch -- and because the
        // `index == 0` arm clears the stream BEFORE hydrating, a connect-time replay that tripped
        // this left the client with an empty stream, permanently, while sends kept working. The
        // id is what callers route on; the name is display only. So resolve to a placeholder and
        // keep the packet. See `data::game::{item,location}_or_placeholder`.
        Ok(LocatedItem {
            item: receiver_game.item_or_placeholder(network.item),
            location: match Location::well_known(network.location) {
                Some(location) => location,
                None => sender_game.location_or_placeholder(network.location),
            },
            sender,
            receiver,
            flags: network.flags,
        })
    }

    /// The item at this location.
    pub fn item(&self) -> Item {
        self.item
    }

    /// The location that contains this item.
    pub fn location(&self) -> Location {
        self.location
    }

    /// The player whose world contains `location`.
    pub fn sender(&self) -> &Player {
        self.sender.as_ref()
    }

    /// The player to whom `item` has been or would be sent.
    pub fn receiver(&self) -> &Player {
        self.receiver.as_ref()
    }

    /// Whether this item can unblock logical advancement.
    pub fn is_progression(&self) -> bool {
        self.flags.contains(NetworkItemFlags::PROGRESSION)
    }

    /// Whether this item is especially useful.
    pub fn is_useful(&self) -> bool {
        self.flags.contains(NetworkItemFlags::USEFUL)
    }

    /// Whether this item is a trap.
    pub fn is_trap(&self) -> bool {
        self.flags.contains(NetworkItemFlags::TRAP)
    }
}

impl fmt::Debug for LocatedItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(
            f,
            "item {} ({}) at location {} ({}) from {} for {}",
            self.item.id(),
            self.item.name(),
            self.location.id(),
            self.location.name(),
            self.sender.alias(),
            self.receiver.alias(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use super::*;
    use crate::protocol::{NetworkItem, NetworkItemFlags, NetworkPlayer, NetworkSlot, SlotType};
    use crate::{Item, Location};

    /// The receiving world. Declares the item ids it can be sent.
    const OURS: &str = "Elden Ring";
    /// A sending world that will name a location outside its own data package.
    const THEIRS: &str = "Geometry Dash";

    /// The id `Geometry Dash` sent us that isn't in `Geometry Dash`'s data package. This is the
    /// literal value from hyp_ER's 2026-08-05 bundle.
    const UNRESOLVABLE: i64 = 130827133;

    fn player(slot: u32, game: &str) -> Arc<Player> {
        Arc::new(
            Player::hydrate(
                NetworkPlayer {
                    team: 0,
                    slot,
                    alias: format!("p{slot}"),
                    name: format!("p{slot}").into(),
                },
                &NetworkSlot {
                    name: format!("p{slot}").into(),
                    game: game.into(),
                    r#type: SlotType::Player,
                    group_members: vec![],
                },
                &HashMap::new(),
            )
            .expect("player hydrates"),
        )
    }

    /// `Elden Ring` knows all three item ids it is sent; `Geometry Dash` declares only one of
    /// the two locations it sends from.
    fn games() -> (Game, Game) {
        let ours = Game::new(
            OURS.into(),
            (1..=3)
                .map(|i| Item::new(7790000 + i, format!("ER item {i}").into(), OURS.into()))
                .collect(),
            vec![],
        );
        let theirs = Game::new(
            THEIRS.into(),
            vec![],
            vec![Location::new(1, "GD Level 1".into(), THEIRS.into())],
        );
        (ours, theirs)
    }

    fn batch() -> Vec<NetworkItem> {
        vec![
            NetworkItem {
                item: 7790001,
                location: 1,
                player: 1,
                flags: NetworkItemFlags::empty(),
            },
            // The poison pill: a real-looking id that `Geometry Dash` never declared.
            NetworkItem {
                item: 7790002,
                location: UNRESOLVABLE,
                player: 1,
                flags: NetworkItemFlags::empty(),
            },
            NetworkItem {
                item: 7790003,
                location: 1,
                player: 1,
                flags: NetworkItemFlags::empty(),
            },
        ]
    }

    /// ⭐ THE MOTIVATING CASE (hyp_ER, 2026-08-05).
    ///
    /// `Client::handle_message` hydrates a `ReceivedItems` batch through a single
    /// `collect::<Result<Vec<_>, _>>`, which short-circuits on the first error. Before the fix,
    /// the middle item below aborted the collect and ALL THREE items were discarded -- and
    /// because the `index == 0` arm clears the stream before hydrating, the client was left
    /// with an empty receive stream permanently, while sends kept working.
    ///
    /// This test drives the same `collect` the client does, so it fails if the short-circuit
    /// ever comes back.
    #[test]
    fn one_unresolvable_location_does_not_discard_the_batch() {
        let (ours, theirs) = games();
        let (sender, receiver) = (player(1, THEIRS), player(2, OURS));

        let hydrated = batch()
            .into_iter()
            .map(|network| {
                LocatedItem::hydrate_with_games(
                    network,
                    sender.clone(),
                    receiver.clone(),
                    &theirs,
                    &ours,
                )
            })
            .collect::<Result<Vec<_>, Error>>()
            .expect("an unresolvable location must not fail the batch");

        assert_eq!(hydrated.len(), 3, "every item in the batch survives");
    }

    /// The count is only half of it: `Client` stores `ReceivedItem::new(item, index + i)` and
    /// consumers index the stream POSITIONALLY (the Elden Ring client walks
    /// `received_items().iter().enumerate()`). So dropping or reordering an entry would
    /// silently misalign every later item against the receive cursor. Order and ids must hold.
    #[test]
    fn the_batch_keeps_its_order_and_ids() {
        let (ours, theirs) = games();
        let (sender, receiver) = (player(1, THEIRS), player(2, OURS));

        let hydrated = batch()
            .into_iter()
            .map(|network| {
                LocatedItem::hydrate_with_games(
                    network,
                    sender.clone(),
                    receiver.clone(),
                    &theirs,
                    &ours,
                )
            })
            .collect::<Result<Vec<_>, Error>>()
            .expect("batch hydrates");

        assert_eq!(
            hydrated.iter().map(|li| li.item().id()).collect::<Vec<_>>(),
            vec![7790001, 7790002, 7790003],
            "items stay in the order the server sent them"
        );
        assert_eq!(
            hydrated
                .iter()
                .map(|li| li.location().id())
                .collect::<Vec<_>>(),
            vec![1, UNRESOLVABLE, 1],
            "the unresolvable id is PRESERVED, not rewritten -- callers route on the id"
        );
    }

    /// The placeholder is display-only sugar: it must never be mistaken for a real name, and
    /// the resolvable neighbours must still get their true names.
    #[test]
    fn the_placeholder_names_itself_as_one() {
        let (ours, theirs) = games();
        let (sender, receiver) = (player(1, THEIRS), player(2, OURS));

        let hydrated = batch()
            .into_iter()
            .map(|network| {
                LocatedItem::hydrate_with_games(
                    network,
                    sender.clone(),
                    receiver.clone(),
                    &theirs,
                    &ours,
                )
            })
            .collect::<Result<Vec<_>, Error>>()
            .expect("batch hydrates");

        assert_eq!(hydrated[0].location().name(), "GD Level 1");
        assert_eq!(
            hydrated[1].location().name(),
            format!("<location #{UNRESOLVABLE}>"),
            "an unresolvable location is visibly a placeholder"
        );
        assert_eq!(hydrated[1].location().game(), THEIRS);
    }

    /// The same short-circuit reaches the SCOUT path (`LocationInfo`), where the roles flip:
    /// the receiver is the foreign world, so it is the ITEM that may not resolve. Elden Ring
    /// scouts all of its locations in one batch, so one foreign item used to void the lot.
    #[test]
    fn one_unresolvable_item_does_not_discard_the_batch() {
        let (ours, theirs) = games();
        let (sender, receiver) = (player(2, OURS), player(1, THEIRS));

        let scouted = LocatedItem::hydrate_with_games(
            NetworkItem {
                item: 999_999_999,
                location: 1,
                player: 1,
                flags: NetworkItemFlags::empty(),
            },
            sender,
            receiver,
            &ours,
            &theirs,
        )
        .expect("an unresolvable item must not fail hydration");

        assert_eq!(scouted.item().id(), 999_999_999, "the id is preserved");
        assert_eq!(scouted.item().name(), "<item #999999999>");
    }
}
