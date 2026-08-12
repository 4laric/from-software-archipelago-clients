//! Trap effects -- the WORDS, the numbers and the arithmetic. No game access lives here.
//!
//! House split, the same one `region_lock` and `marker::refusal_toast` use: er-logic owns what a
//! trap IS and what the player is told, the client crate owns reaching into the game. Everything in
//! this file is host-testable, which matters more for traps than for most features: a trap is a
//! deliberate insult to the player and the difference between "annoying" and "save-ruining" is
//! arithmetic somebody has to be able to read.
//!
//! ## Scope of this module today
//!
//! Two effect traps, neither of which needs new reverse engineering (issue #114 tiers them):
//!
//! * **Rune Thief** -- halve the rune count. One typed call in the client, through `runes.rs` and
//!   its single-writer discipline.
//! * **No Flask** -- the flask heals NOTHING for a while. `changeHpEstusFlaskCorrectRate` and its
//!   MP twin are real `SpEffectParam` columns and vanilla row `12061` already sets both to 0 at
//!   `effectEndurance 5`, `spCategory 0` -- so this is one `apply_speffect` on a row we own, not
//!   the input-hook problem the design originally filed it as.
//!
//! ...plus SPAWN traps, which are open-ended: the world mints the three spawn ids INTO the item
//! name and the client parses them out ([`SpawnSpec`]). 🛑 That is the point of the design -- the
//! client holds NO creature table, so a world that learns a new creature needs no client release,
//! and there is no id list here to drift out of date. `Trap: Runebear` predates it and survives as
//! its own fixed variant because that exact name is already in the wild.
//!
//! 🛑 A trap's DURATION is a param field, not client bookkeeping: `effectEndurance` on the row we
//! apply. No timer, no tick loop, no state machine, and nothing to leak if the player quits mid-trap.
//! That is the finding the whole trap design rests on.

use std::borrow::Cow;

/// The traps this build can fire. `OptionSet` names will mirror these, so 🛑 a name added here later
/// is safe and a name REMOVED is a compat break -- never ship one you might withdraw.
///
/// [`Trap::Spawn`] is the open-ended one: it CARRIES its ids rather than naming a creature this
/// crate knows. `Runebear` stays a variant of its own -- it is a `Spawn` in every respect except
/// its name, and `Trap: Runebear` is already in the wild.
///
/// 🛑 STILL `Copy`, and that is a requirement rather than a convenience: the client's
/// `poll_pending` hands a trap to `fire` BY VALUE and pushes the same trap back onto the queue when
/// the tick refused it. That is the constraint that keeps a spawn's label INLINE in [`SpawnSpec`] --
/// a fixed [`LABEL_CAP`]-byte buffer -- rather than an owned `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    RuneThief,
    NoFlask,
    Runebear,
    /// Put `count` of a creature on the player's head. The ids come from the ITEM NAME.
    Spawn(SpawnSpec),
}

impl Trap {
    /// The yaml/option name. Stable identifier, lower_snake, never localised.
    ///
    /// 🛑 NO LONGER `&'static str`, because a spawn's key is minted from ids that arrive at
    /// runtime; there is no static to borrow. `Cow` rather than `String` so the fixed traps -- the
    /// ones the yaml actually names, and the ones on every hot path -- still cost nothing.
    pub fn key(self) -> Cow<'static, str> {
        match self {
            Trap::RuneThief => Cow::Borrowed("rune_thief"),
            Trap::NoFlask => Cow::Borrowed("no_flask"),
            Trap::Runebear => Cow::Borrowed("runebear"),
            // Carries DIGITS, unlike every key above it. The lower_snake pin in
            // `every_trap_line_is_ascii_and_names_itself` guards `ALL`, which is the yaml surface;
            // a spawn key is a LOG identifier for a trap no option ever names one by one.
            Trap::Spawn(spec) => Cow::Owned(spec.key()),
        }
    }

    /// The line the player sees. ASCII only (`every_trap_line_is_ascii`) -- the in-game font draws
    /// `?` for anything else, and the v0.2.18 em-dash escape lived in a format string's constant
    /// part.
    ///
    /// Phrased as the EFFECT, not the receipt, exactly like the region-unlock line: "you received
    /// Rune Thief" is something the player has to translate; "half your runes are gone" is the
    /// thing that changed about their run.
    pub fn toast(self) -> Cow<'static, str> {
        match self {
            Trap::RuneThief => Cow::Borrowed("TRAP: Rune Thief -- half your runes are gone"),
            // 🛑 Says HEALS NOTHING, not "cannot drink". The charge is still spent -- see
            // `NO_FLASK_SECONDS`. Promising a blocked animation would be a lie the player finds out
            // about at the worst possible moment.
            Trap::NoFlask => Cow::Borrowed("TRAP: No Flask -- your flask heals nothing for 20s"),
            Trap::Runebear => {
                Cow::Borrowed("TRAP: Runebear -- something large is standing where you are")
            }
            // Names WHAT arrived and HOW MANY: "something is standing where you are" alone reads
            // as a bug report -- and so does a raw model number, which is the only other thing this
            // side could say. ASCII by construction: [`SpawnSpec::new`] refuses a non-ASCII label,
            // and the empty-label fallback is `c<chr_id>`, digits only.
            Trap::Spawn(spec) => Cow::Owned(spec.toast()),
        }
    }
}

/// How long `NoFlask` lasts, in seconds, written to the row's `effectEndurance`.
///
/// 20 s is bobler's own ask. It is long enough to lose a fight and short enough that it cannot be
/// mistaken for a permanent break, which matters: the failure mode of getting this wrong is a
/// player who thinks their save is broken.
pub const NO_FLASK_SECONDS: f32 = 20.0;

// 🛑🛑 THE LINE BETWEEN A TRAP AND A SAVE-RUINING BUG, asserted at COMPILE TIME.
//
// `-1` means PERMANENT in this param, and every row in the down palette carries it. A trap that
// shipped a permanent duration would not inconvenience the player, it would end the character --
// so this must fail the BUILD, not a test run. (It began life as a `#[test]`; clippy correctly
// pointed out that an assertion over a `const` is constant, which is the argument for moving it
// here rather than for deleting it.)
const _: () = assert!(
    NO_FLASK_SECONDS > 0.0,
    "a trap with no duration never expires"
);
const _: () = assert!(
    NO_FLASK_SECONDS < 120.0,
    "longer than a boss fight is a broken save, not a trap"
);

/// The flask-healing multiplier `NoFlask` writes. 0.0 = the flask restores nothing.
///
/// Vanilla row `12061` sets exactly this pair, so the column is known-live rather than inferred
/// from its name -- which is the failure that broke enemy scaling once.
pub const NO_FLASK_CORRECT_RATE: f32 = 0.0;

/// The item-name prefix every trap carries. The world mints synthetic items (`ITEMS` with no
/// `ITEM_GRANTS`) and the client recognises them HERE, by name, exactly as it recognises
/// `Boss Key: <Boss>`. That is what keeps traps off the contract entirely -- no slot_data key, no
/// `CONTRACT_HASH` move, no version lockstep.
pub const ITEM_PREFIX: &str = "Trap: ";

impl Trap {
    /// The item name the world mints for this trap.
    ///
    /// 🛑 CROSS-REPO STRING CONTRACT WITH NO GATE BEHIND IT. `greenfield/eldenring/features/traps.py`
    /// carries the same two strings, and its `test_gf_traps` pins them literally. Change one side
    /// and NOTHING breaks: the item still arrives, is still filler, and silently never fires.
    ///
    /// The parameterised arm mints the same shape it parses ([`SpawnSpec::item_name`]), so the
    /// round trip is closed INCLUDING the world's own label ("Basilisk"), which is RETAINED from
    /// the name rather than recovered from the ids (it is not recoverable from them) -- see
    /// [`SpawnSpec::label`] for why it is nonetheless cosmetic.
    pub fn item_name(self) -> Cow<'static, str> {
        match self {
            Trap::RuneThief => Cow::Borrowed("Trap: Rune Thief"),
            Trap::NoFlask => Cow::Borrowed("Trap: No Flask"),
            Trap::Runebear => Cow::Borrowed("Trap: Runebear"),
            Trap::Spawn(spec) => Cow::Owned(spec.item_name()),
        }
    }

    /// The trap a received item name denotes, or `None` for anything else.
    ///
    /// The fixed traps match EXACTLY, not "starts with the prefix": an unknown `Trap: ...` name is
    /// a world newer than this client, and firing the wrong effect would be worse than firing none.
    /// The caller logs it.
    ///
    /// A spawn is the one name-shaped exception, and it is not a relaxation of that rule: the ids
    /// are IN the name, so the name is not a reference to knowledge this client might lack. It is
    /// parsed under [`SpawnSpec::from_item_name`]'s strict rules, which refuse on every doubt.
    pub fn from_item_name(name: &str) -> Option<Self> {
        // Exact match FIRST. `Trap: Runebear` must keep resolving to `Trap::Runebear` and not to
        // some future parse of the same string.
        if let Some(fixed) = ALL.iter().copied().find(|t| t.item_name() == name) {
            return Some(fixed);
        }
        SpawnSpec::from_item_name(name).map(Trap::Spawn)
    }
}

/// Every FIXED trap this build can fire. One place, so a new variant cannot be half-added.
///
/// 🛑 [`Trap::Spawn`] is deliberately NOT here, and the length stays 3. `ALL` is the set of traps
/// with a CONSTANT name -- it is what `from_item_name` exact-matches against, and what the
/// round-trip and ASCII pins iterate. A spawn family is unbounded (any creature the world knows),
/// so it has no enumerable membership to list; its equivalents are the round-trip and refusal
/// tests below.
pub const ALL: [Trap; 3] = [Trap::RuneThief, Trap::NoFlask, Trap::Runebear];

// ---- spawn traps --------------------------------------------------------------------------------

/// What to put on the player's head: a creature model, the two param rows that give it a body and a
/// brain, and how many.
///
/// 🛑 THE IDS TRAVEL IN THE ITEM NAME (see [`Self::from_item_name`]). The client keeps no creature
/// table at all, which is the whole design: `NpcName.fmg` names only 76 of ~600 models, so any
/// client-side table would be both incomplete and a thing to keep in lockstep with the world. A
/// name is a payload, not a lookup key.
///
/// The world's own LABEL travels the same way and is RETAINED, in a fixed [`LABEL_CAP`]-byte
/// buffer. It has to be: the toast is the only thing a spawn trap ever shows the player, and
/// "TRAP: c4150 x3" reads as a bug rather than as a joke.
///
/// `Copy`, because [`Trap`] must be -- the client's queue fires a trap by value and pushes the same
/// value back when the tick refused it. That is what rules out a `String` here and buys the fixed
/// cap instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnSpec {
    /// Character model, e.g. `4150` for `c4150` (basilisk). Bare number, no `c`.
    pub chr_id: i32,
    /// `NpcParam` row -- the body: hp, damage, what it drops.
    pub npc_param_id: i32,
    /// `NpcThinkParam` row -- the brain. Without a live one the creature stands there.
    pub think_param_id: i32,
    /// How many. `1..=MAX_SPAWN_COUNT`.
    pub count: u32,
    /// The world's readable name for the creature, ASCII, the first `label_len` bytes valid.
    ///
    /// 🛑 PRIVATE, and writable ONLY through [`SpawnSpec::new`]. An arbitrary `[u8; LABEL_CAP]` is
    /// not necessarily UTF-8, and [`SpawnSpec::label`] promises never to panic; that promise is
    /// only keepable while one checked constructor owns these bytes.
    label: [u8; LABEL_CAP],
    /// How many of `label` are the label. `0` = none retained -- see [`SpawnSpec::label`].
    label_len: u8,
}

/// The most bytes of label a [`SpawnSpec`] retains.
///
/// 🛑 A CONTRACT WITH THE WORLD, which caps its emitted labels at this same number, and a REFUSAL
/// rule rather than a truncation (see [`SpawnSpec::from_item_name`]). 24 fits every creature name
/// in `NpcName.fmg` and the toast line it lands in; a longer one is a world minting something this
/// client cannot faithfully repeat, and repeating it wrong renames the creature in the one line the
/// player reads.
pub const LABEL_CAP: usize = 24;

// The buffer is indexed by a `u8` length, and an empty label is the fallback path rather than a
// label -- both are properties of the CONSTANT, so both fail the build rather than a test run.
const _: () = assert!(
    LABEL_CAP <= u8::MAX as usize,
    "label_len is a u8 and could not address the buffer"
);
const _: () = assert!(
    LABEL_CAP >= 1,
    "a label cap of zero retains nothing and every toast falls back to a model number"
);

/// The most a single trap may spawn.
///
/// 8 is a horde and not a hang. This is the trap/save-ruining-bug line again, and it is a REFUSAL
/// rule in the parser rather than a clamp: a world that asks for 400 basilisks has a bug, and
/// silently spawning 8 of them would hide it while still ruining the run.
pub const MAX_SPAWN_COUNT: u32 = 8;

// 🛑🛑 ASSERTED AT COMPILE TIME, for the same reason `NO_FLASK_SECONDS` is: the failure mode of
// getting this wrong is not an annoyed player, it is a frame time no controller can play through
// and a save the player abandons. A number that big must fail the BUILD, not a test run somebody
// could `--skip`.
const _: () = assert!(
    MAX_SPAWN_COUNT >= 1,
    "a spawn trap that spawns nothing is not a trap"
);
const _: () = assert!(
    MAX_SPAWN_COUNT <= 8,
    "a horde large enough to hang the game is a save-ruining bug, not a trap"
);

impl SpawnSpec {
    /// `chara_init_param_id` for a spawned creature: none.
    ///
    /// `CharaInitParam` describes a HUMAN loadout (stats, starting equipment); a bear has no use
    /// for one, and -1 is the param convention for unset.
    pub const CHARA_INIT_PARAM_ID: i32 = -1;

    /// Build a spec, checking the label. `None` when the label is longer than [`LABEL_CAP`] bytes
    /// or is not ASCII.
    ///
    /// 🛑 THE ONLY WRITER OF `label`, which is what makes [`Self::label`]'s no-panic promise
    /// keepable: nothing but the ASCII checked here ever reaches the buffer. REFUSES rather than
    /// truncates, for the same reason the parser refuses everything else it doubts -- a truncated
    /// label is a creature renamed in the only line the player sees, and a trap that lies about
    /// what arrived is worse than one that never fires.
    ///
    /// `const`, so [`RUNEBEAR_SPAWN`] can stay a `const` and a built-in label that outgrew the cap
    /// fails the BUILD rather than a session.
    pub const fn new(
        chr_id: i32,
        npc_param_id: i32,
        think_param_id: i32,
        count: u32,
        label: &str,
    ) -> Option<Self> {
        let bytes = label.as_bytes();
        if bytes.len() > LABEL_CAP {
            return None;
        }
        let mut buf = [0u8; LABEL_CAP];
        let mut i = 0;
        while i < bytes.len() {
            // The byte test spelled out rather than `is_ascii()`: the in-game font draws `?` for
            // everything above 0x7f, so this is the font's rule, not a string-type nicety.
            if bytes[i] >= 0x80 {
                return None;
            }
            buf[i] = bytes[i];
            i += 1;
        }
        Some(Self {
            chr_id,
            npc_param_id,
            think_param_id,
            count,
            label: buf,
            // Fits, by the compile-time assert on `LABEL_CAP` and the length check above.
            label_len: bytes.len() as u8,
        })
    }

    /// The retained label exactly as stored, or `""` when none was retained.
    ///
    /// 🛑 CANNOT PANIC, and that is the requirement rather than a style note: this runs in the
    /// RECEIVE path, where a panic costs the player the session and a wrong word costs them a
    /// word. Both the length and the UTF-8 decode FALL BACK instead of unwrapping, even though
    /// [`Self::new`] is the only writer and makes either failure unreachable.
    pub fn retained_label(&self) -> &str {
        let len = (self.label_len as usize).min(LABEL_CAP);
        std::str::from_utf8(&self.label[..len]).unwrap_or("")
    }

    /// The name in the item string and in the toast: the label the WORLD wrote.
    ///
    /// 🛑 COSMETIC, AND ONLY COSMETIC. The world writes a READABLE label ("Basilisk") because the
    /// item shows up in everybody's AP item log and in the line the player is shown; nothing keys
    /// on it, and [`Self::key`] deliberately does not. The client still could not DERIVE it --
    /// `NpcName.fmg` names only 76 of ~600 models -- which is exactly why it travels in the name as
    /// a payload rather than being looked up here.
    ///
    /// `c<chr_id>` survives as a fallback for an EMPTY label ONLY, and it is what this side can
    /// always say truthfully -- the id a bug report needs anyway. 🛑 It is UNREACHABLE through
    /// [`Self::from_item_name`], which refuses an empty label outright; it exists so a hand-built
    /// spec still names something rather than nothing.
    pub fn label(&self) -> Cow<'_, str> {
        let retained = self.retained_label();
        if retained.is_empty() {
            Cow::Owned(format!("c{}", self.chr_id))
        } else {
            Cow::Borrowed(retained)
        }
    }

    /// Log/identifier key. Digits, unlike the fixed traps' `lower_snake` keys.
    ///
    /// 🛑 MINTED FROM THE IDS AND THE COUNT, NEVER FROM THE LABEL, even though the label now
    /// travels with the spec and would read better. A label is cosmetic and RELABELABLE: the world
    /// may rename a creature in any release, and it owes this client nothing when it does. A key is
    /// an IDENTITY surface -- it is what a log line, a triage grep and an issue thread all name the
    /// same trap by -- and an identifier that moves when somebody edits a display string is an
    /// identifier no history can be searched with.
    pub fn key(self) -> String {
        format!("spawn_c{}_x{}", self.chr_id, self.count)
    }

    /// The line the player sees, naming the creature the world named.
    ///
    /// 🛑 THE LABEL, NOT THE MODEL NUMBER. This is the ONLY thing a spawn trap ever shows the
    /// player, and "TRAP: c4150 x3" reads as a bug rather than as a joke -- a confusing toast has
    /// already been reported twice by playtesters. ASCII by construction: [`Self::new`] refuses a
    /// non-ASCII label and the fallback is `c` plus digits.
    pub fn toast(self) -> String {
        format!(
            "TRAP: {} x{} -- something is standing where you are",
            self.label(),
            self.count
        )
    }

    /// The item name this spec round-trips through.
    ///
    /// 🛑 CROSS-REPO STRING CONTRACT, and a wider one than the fixed names: the world mints this
    /// shape for creatures this client has never heard of. `Trap: <label> (<chr>/<npc>/<think>
    /// x<count>)`. Change the shape on one side and every spawn trap in the pool becomes an
    /// unrecognised name that is logged and dropped.
    pub fn item_name(self) -> String {
        format!(
            "{ITEM_PREFIX}{} ({}/{}/{} x{})",
            self.label(),
            self.chr_id,
            self.npc_param_id,
            self.think_param_id,
            self.count
        )
    }

    /// Parse `Trap: <label> (<chr>/<npc>/<think> x<count>)`, or `None`.
    ///
    /// 🛑 STRICT, AND IT REFUSES RATHER THAN GUESSES -- the same rule the exact-match arm follows,
    /// for a stronger reason: these ids go straight to the game's debug creator. A field that is
    /// nearly a number, or a think row belonging to a different creature, is not a name to be
    /// generous about. Every branch below returns `None` on a doubt:
    ///
    /// * prefix `Trap: ` and a trailing `)`;
    /// * the payload opens at the LAST ` (` -- a label may carry one of its own, a payload may not;
    /// * exactly three `/` fields, the third split by exactly one ` x`;
    /// * all four fields parse (`i32`, `i32`, `i32`, `u32`);
    /// * `chr` is a plausible model number, `100..=9999`;
    /// * `npc` and `think` are IN THE `chr` FAMILY (their decimal ids start with its digits) --
    ///   the same check `the_runebear_param_rows_belong_to_its_model` makes, because a body running
    ///   another creature's brain is the failure it catches;
    /// * `count` is `1..=MAX_SPAWN_COUNT`;
    /// * the label is non-empty and ASCII (it reaches the in-game font);
    /// * 🛑 the label is at most [`LABEL_CAP`] bytes -- REFUSED, not truncated, exactly like every
    ///   other malformed payload here. The label is retained INLINE so [`SpawnSpec`] stays `Copy`
    ///   (see [`Self::new`]), so the ceiling is real rather than a preference; the world caps its
    ///   own emitted labels at the same number, which makes a longer one a world this client
    ///   cannot faithfully repeat. Truncating would silently rename the creature in the one line
    ///   the player reads, and a trap that lies about what arrived is worse than one that never
    ///   fires.
    pub fn from_item_name(name: &str) -> Option<Self> {
        let body = name.strip_prefix(ITEM_PREFIX)?.strip_suffix(')')?;
        // LAST ` (`, so a label carrying one of its own cannot swallow the payload.
        let (label, payload) = body.rsplit_once(" (")?;
        if label.is_empty() || !label.is_ascii() {
            return None;
        }

        // EXACTLY three, checked rather than taken from the front: `split` yields the first three
        // of four just as happily, and the fourth would be silently discarded.
        let mut fields = payload.split('/');
        let chr = fields.next()?;
        let npc = fields.next()?;
        let tail = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        let mut halves = tail.split(" x");
        let think = halves.next()?;
        let count = halves.next()?;
        if halves.next().is_some() {
            return None;
        }

        // Through the CHECKED constructor, which is where the label cap is enforced: a label the
        // buffer cannot hold whole is refused here, exactly like a count the game cannot survive.
        let spec = SpawnSpec::new(
            chr.parse().ok()?,
            npc.parse().ok()?,
            think.parse().ok()?,
            count.parse().ok()?,
            label,
        )?;
        spec.is_sane().then_some(spec)
    }

    /// The id and range rules, split out so the parser reads as a shape check and this reads as a
    /// safety check. Both halves have to hold.
    fn is_sane(self) -> bool {
        if !(100..=9999).contains(&self.chr_id) {
            return false;
        }
        if !(1..=MAX_SPAWN_COUNT).contains(&self.count) {
            return false;
        }
        let family = self.chr_id.to_string();
        self.npc_param_id.to_string().starts_with(&family)
            && self.think_param_id.to_string().starts_with(&family)
    }
}

// ---- Runebear -----------------------------------------------------------------------------------
//
// DERIVED 2026-08-10 from `gen_inputs.db`, not recalled -- and the derivation corrected a confident
// wrong memory (I had "Runebear is c4300"; it is not).
//
// `msg/item-msgbnd-dcx/NpcName.fmg.xml` ids encode the model as `90` + <model4> + <variant3>:
//   904630310 = "Runebear"  =>  model c4630
// Corroborated by two further tables, which is why this is an id and not a guess:
//   * NpcParam      `4630xxxx` -- 21 rows, all hp 2585, getSoul rising with the area tier
//   * NpcThinkParam `46300000 / 46300010 / 46300020 / 46300052`
//
// ⚠️ `NpcParam.Name` is EMPTY in this dump (7039 rows, zero non-empty) and `nameId` is NOT the
// NpcName id -- joining on it returns nothing, silently. The id-prefix decode is the working route;
// do not "fix" it into a join.

/// Character model: `c4630`.
pub const RUNEBEAR_CHR_ID: i32 = 4630;

/// The NpcParam row the spawn uses.
///
/// `46300010` rather than the family's `...0000` template: every row shares `hp 2585` (the
/// difficulty spread lives in the area-tier speffect ladder, not here) and the template carries
/// `getSoul 0`, so it would pay nothing. A player who survives the bear should be paid for it.
pub const RUNEBEAR_NPC_PARAM_ID: i32 = 46_300_010;

/// The think (AI) row -- the family's base entry. The bear has to actually come after you.
pub const RUNEBEAR_THINK_PARAM_ID: i32 = 46_300_000;

/// `chara_init_param_id` for a non-humanoid: none. Kept as its own name because the client imports
/// it and a pinned test names it; the rationale now lives on [`SpawnSpec::CHARA_INIT_PARAM_ID`],
/// which every spawn -- bear or not -- uses.
pub const RUNEBEAR_CHARA_INIT_PARAM_ID: i32 = SpawnSpec::CHARA_INIT_PARAM_ID;

/// The Runebear as a [`SpawnSpec`], so the legacy variant and a parameterised one reach the game
/// through ONE code path. `count: 1` -- one bear was the ask, and the name it ships under says
/// "something large", singular.
///
/// It carries the label "Runebear" so that `Trap::Spawn(RUNEBEAR_SPAWN)` names the creature the way
/// the fixed [`Trap::Runebear`] line does. 🛑 The bare name `Trap: Runebear` still resolves to the
/// fixed variant -- the exact-match arm wins before the parser ever sees it.
pub const RUNEBEAR_SPAWN: SpawnSpec = match SpawnSpec::new(
    RUNEBEAR_CHR_ID,
    RUNEBEAR_NPC_PARAM_ID,
    RUNEBEAR_THINK_PARAM_ID,
    1,
    "Runebear",
) {
    Some(spec) => spec,
    // 🛑 A BUILD FAILURE, not a runtime fallback. The only way `new` refuses a literal is a label
    // that outgrew `LABEL_CAP` or stopped being ASCII, and either is an edit somebody must see.
    None => panic!("the Runebear label does not fit LABEL_CAP"),
};

/// Rune Thief's new total: half, rounded down.
///
/// Saturating by construction (`u32 / 2`), so there is no underflow branch to get wrong and a
/// player at 0 or 1 rune simply stays where they are. Split out from the client purely so the
/// arithmetic can be read and tested without a game.
pub fn rune_thief_target(current: u32) -> u32 {
    current / 2
}

/// A trap that arrived while the player could not receive it.
///
/// 🛑 WHY A QUEUE AND NOT A RETURN VALUE. Fired from a HOTKEY, "cannot act right now" is fine: the
/// player presses the key again. Fired from an ITEM, it is a LOSS -- the item is already marked
/// received, the server will never resend it, and a trap that quietly evaporated is indistinguishable
/// from a trap that was never in the pool. Issue #114 rule 2: never fire while the player is not in
/// control, DEFER with a starvation cap, and never cancel.
///
/// Deliberately NOT a timer. It holds names and one clock reading; the caller polls it on the tick
/// it already runs, which is the same shape `attunement_replay` uses for the deferred boss payout.
#[derive(Debug, Default)]
pub struct TrapQueue {
    pending: Vec<Trap>,
    /// When the head of the queue started waiting, for [`Self::overdue`]. `None` when empty.
    waiting_since_ms: Option<u64>,
}

/// How long a trap may sit undeliverable before the client says so out loud, in ms.
///
/// Mirrors the boss-defer cap. It is a REPORTING threshold, not a deadline: nothing is dropped when
/// it passes, because the alternative to holding is losing the item outright.
pub const DEFER_WARN_MS: u64 = 30_000;

impl TrapQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a trap for delivery. FIFO -- two traps in one batch fire in the order they arrived.
    pub fn push(&mut self, trap: Trap, now_ms: u64) {
        if self.pending.is_empty() {
            self.waiting_since_ms = Some(now_ms);
        }
        self.pending.push(trap);
    }

    /// The next trap to fire, or `None` while the player cannot receive one.
    ///
    /// `can_fire` is the CALLER's judgement (in world, alive, settled) -- er-logic does not reach
    /// into the game to form it. One per poll, so a batch of five does not land as one event the
    /// player cannot parse.
    pub fn poll(&mut self, now_ms: u64, can_fire: bool) -> Option<Trap> {
        if !can_fire || self.pending.is_empty() {
            return None;
        }
        let trap = self.pending.remove(0);
        self.waiting_since_ms = (!self.pending.is_empty()).then_some(now_ms);
        Some(trap)
    }

    /// Has the head waited longer than [`DEFER_WARN_MS`]? For ONE log line, not for dropping.
    pub fn overdue(&self, now_ms: u64) -> bool {
        match self.waiting_since_ms {
            Some(t) => now_ms.saturating_sub(t) >= DEFER_WARN_MS,
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rune_thief_halves_and_never_underflows() {
        assert_eq!(rune_thief_target(1_000), 500);
        assert_eq!(rune_thief_target(1), 0);
        assert_eq!(rune_thief_target(0), 0);
        assert_eq!(rune_thief_target(u32::MAX), u32::MAX / 2);
    }

    /// 🛑 A trap may impoverish the player; it may never ENRICH them. A sign error here is the one
    /// mistake in this file that would be reported as a cheat rather than as a bug.
    #[test]
    fn rune_thief_never_gives_runes() {
        for n in [0u32, 1, 2, 3, 7, 999, 1_000_000, u32::MAX] {
            assert!(rune_thief_target(n) <= n, "{n} -> {}", rune_thief_target(n));
        }
    }

    /// The duration property is asserted at COMPILE TIME beside the constant (see
    /// `NO_FLASK_SECONDS`), because a save-ruining value should fail the BUILD rather than a test
    /// somebody could skip. This case only pins that the constant is the one we documented.
    #[test]
    fn no_flask_duration_is_the_documented_twenty_seconds() {
        assert_eq!(NO_FLASK_SECONDS, 20.0);
    }

    #[test]
    fn no_flask_rate_heals_nothing() {
        assert_eq!(NO_FLASK_CORRECT_RATE, 0.0);
    }

    #[test]
    fn every_trap_line_is_ascii_and_names_itself() {
        // WITNESS, and a deliberate pin: an empty list would make every assertion below vacuously
        // true, and a NEW trap should force somebody to look at this file rather than sail past it.
        assert_eq!(
            ALL.len(),
            3,
            "a trap was added -- check its line and key here, then bump this"
        );
        for t in ALL {
            assert!(t.toast().is_ascii(), "non-ASCII trap line: {}", t.toast());
            assert!(t.key().is_ascii());
            assert!(
                t.toast().starts_with("TRAP: "),
                "{} must announce itself",
                t.key()
            );
            assert!(
                t.key().chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{}",
                t.key()
            );
        }
    }

    /// Keys are the yaml surface; two traps sharing one is an option that cannot address them both.
    #[test]
    fn the_runebear_ids_are_the_derived_ones() {
        // Pinned against a careless edit. These are load-bearing GAME ids: a typo spawns a
        // different creature, or nothing, and the failure surfaces in a live session rather than as
        // a build error. The derivation sits in the comment above them (FMG id 904630310).
        assert_eq!(RUNEBEAR_CHR_ID, 4630);
        assert_eq!(RUNEBEAR_NPC_PARAM_ID, 46_300_010);
        assert_eq!(RUNEBEAR_THINK_PARAM_ID, 46_300_000);
        assert_eq!(RUNEBEAR_CHARA_INIT_PARAM_ID, -1);
    }

    /// The npc and think rows must belong to the model `chr_id` names, or we spawn one creature's
    /// body running another's brain.
    #[test]
    fn the_runebear_param_rows_belong_to_its_model() {
        let prefix = RUNEBEAR_CHR_ID.to_string();
        for id in [RUNEBEAR_NPC_PARAM_ID, RUNEBEAR_THINK_PARAM_ID] {
            assert!(
                id.to_string().starts_with(&prefix),
                "param row {id} is not in the c{prefix} family"
            );
        }
    }

    #[test]
    fn trap_keys_are_unique() {
        let keys: Vec<String> = ALL.iter().map(|t| t.key().into_owned()).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate trap key in {keys:?}");
    }
    // ---- the queue -----------------------------------------------------------------------------

    #[test]
    fn a_trap_that_cannot_fire_is_held_not_dropped() {
        let mut q = TrapQueue::new();
        q.push(Trap::RuneThief, 0);
        // Not in world for a full minute: polled repeatedly, never delivered, never lost.
        for t in (0..60_000).step_by(1_000) {
            assert_eq!(q.poll(t, false), None);
        }
        assert_eq!(
            q.len(),
            1,
            "the trap was dropped -- the server will never resend it"
        );
        assert_eq!(q.poll(60_000, true), Some(Trap::RuneThief));
        assert!(q.is_empty());
    }

    #[test]
    fn traps_fire_in_arrival_order_one_per_poll() {
        let mut q = TrapQueue::new();
        q.push(Trap::RuneThief, 0);
        q.push(Trap::NoFlask, 0);
        // One per poll: a batch must not land as a single unreadable event.
        assert_eq!(q.poll(10, true), Some(Trap::RuneThief));
        assert_eq!(q.poll(10, true), Some(Trap::NoFlask));
        assert_eq!(q.poll(10, true), None);
    }

    #[test]
    fn overdue_reports_and_does_not_drop() {
        let mut q = TrapQueue::new();
        assert!(!q.overdue(1_000_000), "an empty queue is never overdue");
        q.push(Trap::NoFlask, 1_000);
        assert!(!q.overdue(1_000 + DEFER_WARN_MS - 1));
        assert!(q.overdue(1_000 + DEFER_WARN_MS));
        // 🛑 The cap is a REPORTING threshold. Passing it must not lose the trap.
        assert_eq!(q.len(), 1);
        assert_eq!(q.poll(9_999_999, true), Some(Trap::NoFlask));
    }

    #[test]
    fn the_wait_clock_restarts_for_the_next_trap_in_line() {
        let mut q = TrapQueue::new();
        q.push(Trap::RuneThief, 0);
        q.push(Trap::NoFlask, 0);
        assert_eq!(q.poll(DEFER_WARN_MS, true), Some(Trap::RuneThief));
        // The survivor has only just reached the head; it is not instantly overdue on the old clock.
        assert!(
            !q.overdue(DEFER_WARN_MS),
            "the second trap inherited the first one's wait"
        );
    }

    // ---- the cross-repo name contract ----------------------------------------------------------

    /// 🛑 THE STRINGS `greenfield/eldenring/features/traps.py` MINTS. Nothing enforces this across
    /// the repo boundary -- change one side and the item silently never fires -- so both sides pin
    /// the literals and `test_gf_traps` is the other half of this test.
    #[test]
    fn item_names_are_the_ones_the_world_mints() {
        assert_eq!(Trap::RuneThief.item_name(), "Trap: Rune Thief");
        assert_eq!(Trap::NoFlask.item_name(), "Trap: No Flask");
    }

    #[test]
    fn every_trap_round_trips_through_its_item_name() {
        assert_eq!(ALL.len(), 3, "WITNESS: nothing was swept");
        for t in ALL {
            assert_eq!(Trap::from_item_name(&t.item_name()), Some(t));
            assert!(t.item_name().starts_with(ITEM_PREFIX), "{}", t.item_name());
            assert!(t.item_name().is_ascii());
        }
    }

    /// An unknown `Trap: ...` is a WORLD NEWER THAN THIS CLIENT. Refusing is right: firing the
    /// wrong effect would be worse than firing none, and the caller logs the name.
    #[test]
    fn an_unknown_trap_name_is_refused_not_guessed() {
        assert_eq!(Trap::from_item_name("Trap: Reversed Controls"), None);
        assert_eq!(Trap::from_item_name("Boss Key: Godrick"), None);
        assert_eq!(Trap::from_item_name("Smithing Stone [1]"), None);
        assert_eq!(Trap::from_item_name(""), None);
    }

    // ---- spawn traps ---------------------------------------------------------------------------

    /// Fixture builder. Every spec below is built through the CHECKED constructor because that is
    /// the only way the label can be set at all -- the buffer is private -- which also means a
    /// fixture that outgrew `LABEL_CAP` fails loudly here rather than quietly parsing differently.
    fn spec(
        chr_id: i32,
        npc_param_id: i32,
        think_param_id: i32,
        count: u32,
        label: &str,
    ) -> SpawnSpec {
        SpawnSpec::new(chr_id, npc_param_id, think_param_id, count, label)
            .expect("fixture label must be ASCII and fit LABEL_CAP")
    }

    /// The ids reach the game's debug creator, so the name they travel in has to survive the round
    /// trip byte for byte. A spec that comes back changed spawns a DIFFERENT creature, and the
    /// failure surfaces in a live session rather than as a build error.
    ///
    /// The LABEL has to survive it too, and is asserted separately below rather than left to the
    /// derived `PartialEq`: the defect this catches is a parser that VALIDATES the label and then
    /// discards it, which leaves every spec equal to a differently-named one.
    #[test]
    fn a_spawn_spec_round_trips_through_its_item_name() {
        let specs = [
            // Basilisk, the motivating case (issue #114 / the trap the world mints first).
            spec(4150, 41_500_060, 41_500_000, 3, "Basilisk"),
            // A label with a SPACE in it, which is also the shape that would let a payload be
            // swallowed if the split ever moved off the LAST ` (`.
            spec(3210, 32_100_000, 32_100_000, 1, "Giant Crab"),
            // Both ends of the count range, which is where an off-by-one would live.
            SpawnSpec {
                count: MAX_SPAWN_COUNT,
                ..RUNEBEAR_SPAWN
            },
            RUNEBEAR_SPAWN,
            // A three-digit model: the family prefix check is a STRING prefix, so a shorter chr id
            // is the case that would wrongly pass or wrongly fail it.
            spec(100, 10_000_000, 10_000_000, 2, "c100"),
        ];
        // WITNESS: an empty list would make every assertion below vacuously true.
        assert_eq!(specs.len(), 5, "the round-trip corpus was emptied");
        for spec in specs {
            let name = spec.item_name();
            let Some(Trap::Spawn(back)) = Trap::from_item_name(&name) else {
                panic!("{name} did not come back as a spawn at all");
            };
            assert_eq!(
                back, spec,
                "{name} did not come back as the spec that minted it"
            );
            assert_eq!(back.label(), spec.label(), "{name} lost its label");
            assert!(name.starts_with(ITEM_PREFIX), "{name}");
            assert!(name.is_ascii(), "{name}");
        }
    }

    /// The world mints a READABLE label ("Basilisk"); this client mints `c4150` when it has none.
    /// Both must parse to the SAME IDS, or the trap the world actually sends is the one that never
    /// fires.
    #[test]
    fn the_worlds_own_label_parses_to_the_ids_it_carries() {
        let basilisk = spec(4150, 41_500_060, 41_500_000, 3, "Basilisk");
        assert_eq!(
            Trap::from_item_name("Trap: Basilisk (4150/41500060/41500000 x3)"),
            Some(Trap::Spawn(basilisk))
        );
        assert_eq!(
            Trap::from_item_name("Trap: c3210 (3210/32100000/32100000 x1)"),
            Some(Trap::Spawn(spec(3210, 32_100_000, 32_100_000, 1, "c3210")))
        );
        // A label with spaces and punctuation is still just a label -- the payload opens at the
        // LAST ` (`, which is what makes that safe. 🛑 It parses to the same IDS as the basilisk
        // above and NOT to the same spec: the label is retained, so it is part of the value.
        let crab = spec(4150, 41_500_060, 41_500_000, 3, "Giant Crab (Ruin)");
        assert_eq!(
            Trap::from_item_name("Trap: Giant Crab (Ruin) (4150/41500060/41500000 x3)"),
            Some(Trap::Spawn(crab))
        );
        assert_eq!(crab.chr_id, basilisk.chr_id);
        assert_eq!(crab.npc_param_id, basilisk.npc_param_id);
        assert_eq!(crab.think_param_id, basilisk.think_param_id);
        assert_eq!(crab.count, basilisk.count);
    }

    /// 🛑 THE LABEL MUST SURVIVE THE PARSE. The parser validated the label and then DISCARDED it,
    /// so every spawn toast read "TRAP: c4150 x3 -- something is standing where you are". A raw
    /// model number is the only thing the player ever sees of a spawn trap and it reads as a bug;
    /// a confusing toast has already been reported twice. This is the test that catches it coming
    /// back.
    #[test]
    fn a_parsed_spawn_keeps_the_label_the_world_wrote() {
        let cases = [
            ("Trap: Basilisk (4150/41500060/41500000 x3)", "Basilisk"),
            // A label containing a SPACE, and one containing a space AND the payload's own ` (`.
            ("Trap: Giant Crab (4150/41500060/41500000 x2)", "Giant Crab"),
            (
                "Trap: Giant Crab (Ruin) (4150/41500060/41500000 x3)",
                "Giant Crab (Ruin)",
            ),
            ("Trap: c3210 (3210/32100000/32100000 x1)", "c3210"),
        ];
        // WITNESS: an empty list would prove nothing about any label at all.
        assert_eq!(cases.len(), 4, "the label corpus was emptied");
        for (name, want) in cases {
            let Some(Trap::Spawn(parsed)) = Trap::from_item_name(name) else {
                panic!("{name} was refused outright");
            };
            assert_eq!(parsed.label(), want, "{name} lost its label");
            assert_eq!(
                parsed.retained_label(),
                want,
                "{name} did not RETAIN its label -- `label()` fell back"
            );
            // The player-facing half: the toast is the whole point of retaining it.
            let line = Trap::Spawn(parsed).toast();
            assert!(line.contains(want), "{line} does not name the creature");
            // ...and the name it mints back carries the label, so the round trip is closed on it.
            assert!(parsed.item_name().contains(want), "{}", parsed.item_name());
        }

        // The defect itself, pinned as the WHOLE line rather than as a `contains`: what shipped was
        // "TRAP: c4150 x3 -- something is standing where you are". (A `c<chr>` LABEL is legitimate
        // and one of the cases above, which is why this pin is written out here instead of as a
        // "does not contain the model number" assertion in the loop.)
        let Some(Trap::Spawn(basilisk)) =
            Trap::from_item_name("Trap: Basilisk (4150/41500060/41500000 x3)")
        else {
            panic!("the motivating case was refused outright");
        };
        assert_eq!(
            Trap::Spawn(basilisk).toast(),
            "TRAP: Basilisk x3 -- something is standing where you are"
        );
    }

    /// 🛑 THE CAP IS A REFUSAL, NOT A TRUNCATION, and this is its boundary. Both sides are asserted
    /// -- exactly `LABEL_CAP` ACCEPTED, one byte more REFUSED -- so the refusal is for the RIGHT
    /// reason (the cap) rather than because the whole shape stopped parsing. The number is a
    /// contract with the world, which caps its emitted labels at the same one; truncating instead
    /// would rename the creature in the only line the player reads.
    #[test]
    fn a_label_of_exactly_label_cap_is_kept_and_one_byte_more_is_refused() {
        let at_cap = "L".repeat(LABEL_CAP);
        let over = "L".repeat(LABEL_CAP + 1);
        // WITNESS for the two strings themselves: `repeat` is the only thing standing between this
        // test and asserting the cap against a string that never reached it.
        assert_eq!(at_cap.len(), LABEL_CAP);
        assert_eq!(over.len(), LABEL_CAP + 1);

        let accepted = format!("Trap: {at_cap} (4150/41500060/41500000 x3)");
        let Some(Trap::Spawn(parsed)) = Trap::from_item_name(&accepted) else {
            panic!("{accepted} was refused AT the cap -- the boundary is off by one");
        };
        // Kept WHOLE. A silent truncation would pass a mere `starts_with`.
        assert_eq!(parsed.label(), at_cap.as_str());
        assert_eq!(parsed.retained_label().len(), LABEL_CAP);

        let refused = format!("Trap: {over} (4150/41500060/41500000 x3)");
        assert_eq!(
            Trap::from_item_name(&refused),
            None,
            "a label one byte over LABEL_CAP was accepted -- it can only have been truncated"
        );

        // The constructor is the rule's real home; the parser only inherits it.
        assert!(SpawnSpec::new(4150, 41_500_060, 41_500_000, 3, &at_cap).is_some());
        assert!(SpawnSpec::new(4150, 41_500_060, 41_500_000, 3, &over).is_none());
        // A label is bytes, not characters, and the cap is a BYTE cap -- non-ASCII is refused
        // outright, so there is no multi-byte label that could sneak past a `chars().count()`.
        assert!(SpawnSpec::new(4150, 41_500_060, 41_500_000, 3, "Basilisqu\u{e9}").is_none());
    }

    /// 🛑 A KEY IS AN IDENTITY SURFACE, A LABEL IS COSMETIC. The world may rename a creature in any
    /// release and owes this client nothing when it does. If `key()` moved with the label, every
    /// log line and every triage grep for a trap would move with it. This catches somebody
    /// "improving" the key to read nicely.
    #[test]
    fn the_key_does_not_move_when_only_the_label_changes() {
        let renamed = [
            spec(4150, 41_500_060, 41_500_000, 3, "Basilisk"),
            spec(4150, 41_500_060, 41_500_000, 3, "Basilisk (Ruin)"),
            spec(4150, 41_500_060, 41_500_000, 3, "c4150"),
        ];
        // WITNESS: an empty list would agree with itself about every key it never checked.
        assert_eq!(renamed.len(), 3, "the rename corpus was emptied");
        for s in renamed {
            assert_eq!(
                Trap::Spawn(s).key(),
                "spawn_c4150_x3",
                "the key followed the label {}",
                s.label()
            );
        }
        // ...and the labels really were different, or the loop above is vacuous a second way.
        assert_ne!(renamed[0].label(), renamed[1].label());
        assert_ne!(renamed[1].label(), renamed[2].label());
        // The key DOES move when the IDENTITY moves -- same label, different creature and count.
        assert_ne!(
            Trap::Spawn(spec(4630, 46_300_010, 46_300_000, 3, "Basilisk")).key(),
            "spawn_c4150_x3"
        );
        assert_ne!(
            Trap::Spawn(spec(4150, 41_500_060, 41_500_000, 2, "Basilisk")).key(),
            "spawn_c4150_x3"
        );
    }

    /// 🛑 THE REFUSAL RULES, one case per rule. These ids go straight to the debug creator: a body
    /// running another creature's brain, a count that hangs the frame, or a field that is nearly a
    /// number are all failures that surface in somebody's live session. Refusing costs one trap;
    /// guessing costs a run.
    #[test]
    fn a_malformed_spawn_name_is_refused_not_guessed() {
        let refused = [
            // no `Trap: ` prefix
            "Basilisk (4150/41500060/41500000 x3)",
            // no trailing `)`
            "Trap: Basilisk (4150/41500060/41500000 x3",
            // no ` (` delimiter at all
            "Trap: Basilisk 4150/41500060/41500000 x3)",
            // empty label
            "Trap:  (4150/41500060/41500000 x3)",
            // non-ASCII label -- the in-game font draws `?` for it
            "Trap: Basilisqu\u{e9} (4150/41500060/41500000 x3)",
            // two payload fields, not three
            "Trap: X (4150/41500060 x3)",
            // four payload fields -- the extra one would be silently dropped
            "Trap: X (4150/41500060/41500000/7 x3)",
            // no ` x<count>` at all
            "Trap: X (4150/41500060/41500000)",
            // two ` x` splits
            "Trap: X (4150/41500060/41500000 x3 x4)",
            // chr is not a number
            "Trap: X (c4150/41500060/41500000 x3)",
            // npc is not a number
            "Trap: X (4150/four/41500000 x3)",
            // count is not a number
            "Trap: X (4150/41500060/41500000 xthree)",
            // chr below the plausible model range
            "Trap: X (99/990000/990000 x1)",
            // chr above it
            "Trap: X (10000/100000000/100000000 x1)",
            // negative chr
            "Trap: X (-4150/-41500060/-41500000 x1)",
            // 🛑 npc row from ANOTHER family: a basilisk body running a runebear's stat block
            "Trap: X (4150/46300010/41500000 x3)",
            // 🛑 think row from another family: the body is right, the brain is a bear's
            "Trap: X (4150/41500060/46300000 x3)",
            // count 0 -- a trap that spawns nothing is a trap that looks broken
            "Trap: X (4150/41500060/41500000 x0)",
            // negative count (parses as i32, must not parse as u32)
            "Trap: X (4150/41500060/41500000 x-1)",
        ];
        // WITNESS: one rule per case, and an empty list would refuse nothing at all.
        assert_eq!(refused.len(), 19, "a refusal rule lost its case");
        for name in refused {
            assert_eq!(
                Trap::from_item_name(name),
                None,
                "{name} was accepted -- the parser guessed"
            );
        }
        // Built from the constant, so raising the cap cannot leave this pinned to a stale number.
        let over = format!("Trap: X (4150/41500060/41500000 x{})", MAX_SPAWN_COUNT + 1);
        assert_eq!(
            Trap::from_item_name(&over),
            None,
            "{over} exceeded the cap and was accepted anyway"
        );
        // ...and the value one below it is accepted, so the case above is refused for the RIGHT
        // reason rather than because the whole shape stopped parsing.
        let at_cap = format!("Trap: X (4150/41500060/41500000 x{MAX_SPAWN_COUNT})");
        assert!(Trap::from_item_name(&at_cap).is_some(), "{at_cap}");
        // The LABEL CAP is the one refusal rule whose case cannot be a literal here -- it is built
        // from `LABEL_CAP`. It lives in `a_label_of_exactly_label_cap_is_kept_and_one_byte_more_
        // is_refused`, beside the acceptance that proves it refuses for the right reason.
    }

    /// The cap is asserted at COMPILE TIME beside the constant (see `MAX_SPAWN_COUNT`), because a
    /// horde that hangs the game should fail the BUILD. This case only pins that the constant is
    /// the one we documented -- and it is a NUMBER THE WORLD ALSO KNOWS, since the world must not
    /// mint a name this client will refuse.
    #[test]
    fn max_spawn_count_is_the_documented_eight() {
        assert_eq!(MAX_SPAWN_COUNT, 8);
    }

    /// 🛑 A COMPAT PIN. These three names are already in the wild; a world that shipped before the
    /// parameterised form must keep working against a client that shipped after it. Removing one
    /// would be a compat break, and the way it would show up is a trap that silently never fires.
    #[test]
    fn the_legacy_three_names_still_resolve_to_their_fixed_variants() {
        assert_eq!(
            Trap::from_item_name("Trap: Rune Thief"),
            Some(Trap::RuneThief)
        );
        assert_eq!(Trap::from_item_name("Trap: No Flask"), Some(Trap::NoFlask));
        // 🛑 NOT `Trap::Spawn(RUNEBEAR_SPAWN)`: the bare name has no payload to parse, and the
        // exact-match arm must win before the parser ever sees it.
        assert_eq!(Trap::from_item_name("Trap: Runebear"), Some(Trap::Runebear));
    }

    /// The in-game font draws `?` for anything non-ASCII, and a toast that does not say what
    /// arrived reads as a bug rather than as a trap. Same rule the fixed traps are held to; `ALL`
    /// cannot cover this one because a spawn family has no enumerable membership.
    #[test]
    fn every_spawn_line_is_ascii_and_names_itself() {
        let specs = [
            spec(4150, 41_500_060, 41_500_000, 3, "Basilisk"),
            RUNEBEAR_SPAWN,
            SpawnSpec {
                count: MAX_SPAWN_COUNT,
                ..RUNEBEAR_SPAWN
            },
            // A label at exactly `LABEL_CAP`, with spaces: the widest line the parser can hand the
            // in-game font.
            spec(4150, 41_500_060, 41_500_000, 8, "Ancestral Follower Chief"),
            // No label retained: the only path on which the `c<chr_id>` fallback is reachable, and
            // it must still be ASCII and still name something.
            SpawnSpec::new(4150, 41_500_060, 41_500_000, 1, "")
                .expect("an empty label is constructible even though the parser refuses one"),
        ];
        // WITNESS: an empty list would satisfy every assertion below without checking a line.
        assert_eq!(specs.len(), 5, "the toast corpus was emptied");
        for spec in specs {
            let trap = Trap::Spawn(spec);
            assert!(
                trap.toast().is_ascii(),
                "non-ASCII trap line: {}",
                trap.toast()
            );
            assert!(
                trap.toast().starts_with("TRAP: "),
                "{} must announce itself",
                trap.key()
            );
            assert!(
                trap.toast().contains(&format!("x{}", spec.count)),
                "{} does not say how many arrived",
                trap.toast()
            );
            // 🛑 NAMES THE CREATURE, not the model number: the toast is all the player sees.
            assert!(
                trap.toast().contains(&*spec.label()),
                "{} does not name what arrived",
                trap.toast()
            );
            assert!(trap.key().is_ascii(), "{}", trap.key());
        }
    }

    /// The `c<chr_id>` fallback is unreachable through the parser (an empty label is refused), so
    /// it needs a DIRECT call: a guard no corpus can fire is a guard nothing tests. It exists so a
    /// hand-built spec names something truthful rather than nothing at all.
    #[test]
    fn an_unlabelled_spec_falls_back_to_the_model_number() {
        let bare = SpawnSpec::new(4150, 41_500_060, 41_500_000, 1, "")
            .expect("an empty label is not itself a construction error");
        assert_eq!(bare.retained_label(), "", "nothing was retained");
        assert_eq!(bare.label(), "c4150");
        assert!(Trap::Spawn(bare).toast().contains("c4150"));
        // ...and the parser really does refuse the name it mints back, which is what makes the
        // fallback unreachable from a received item rather than merely unlikely.
        assert_eq!(
            Trap::from_item_name("Trap:  (4150/41500060/41500000 x1)"),
            None
        );
    }

    /// The legacy variant and the parameterised one must describe the SAME bear, or "Trap:
    /// Runebear" and the ids a world would mint for a runebear spawn two different creatures.
    #[test]
    fn the_runebear_spawn_is_the_derived_runebear() {
        assert_eq!(RUNEBEAR_SPAWN.chr_id, RUNEBEAR_CHR_ID);
        assert_eq!(RUNEBEAR_SPAWN.npc_param_id, RUNEBEAR_NPC_PARAM_ID);
        assert_eq!(RUNEBEAR_SPAWN.think_param_id, RUNEBEAR_THINK_PARAM_ID);
        assert_eq!(RUNEBEAR_SPAWN.count, 1, "one bear was the ask");
        // It must also survive the parser's own rules: the legacy trap is not exempt from the
        // family and range checks the parameterised ones are held to.
        assert_eq!(
            Trap::from_item_name(&RUNEBEAR_SPAWN.item_name()),
            Some(Trap::Spawn(RUNEBEAR_SPAWN))
        );
    }

    /// `CharaInitParam` is a HUMAN loadout; -1 is the param convention for unset. Pinned because
    /// the client passes it to every spawn, humanoid or not, and a real row id here would hand a
    /// basilisk a starting weapon and a stat block it was never meant to have.
    #[test]
    fn chara_init_is_unset_for_every_spawn() {
        assert_eq!(SpawnSpec::CHARA_INIT_PARAM_ID, -1);
        assert_eq!(RUNEBEAR_CHARA_INIT_PARAM_ID, SpawnSpec::CHARA_INIT_PARAM_ID);
    }

    /// A parameterised trap arrives as an ITEM, so it goes through the queue like any other -- and
    /// the queue holds `Trap` by value. This is the case that would fail if `Trap` stopped being
    /// `Copy` or the queue grew a fixed-variant assumption.
    #[test]
    fn a_spawn_survives_the_queue_intact() {
        let spec = spec(4150, 41_500_060, 41_500_000, 3, "Basilisk");
        let mut q = TrapQueue::new();
        q.push(Trap::Spawn(spec), 0);
        q.push(Trap::RuneThief, 0);
        assert_eq!(q.poll(0, false), None, "delivered while it could not fire");
        assert_eq!(q.len(), 2, "a held trap was dropped");
        assert_eq!(q.poll(10, true), Some(Trap::Spawn(spec)));
        assert_eq!(q.poll(10, true), Some(Trap::RuneThief));
        assert!(q.is_empty());
    }
}
