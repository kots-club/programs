//! # THE hieroglyph registry — the single source of truth
//!
//! Every fact about a hieroglyph lives here and nowhere else: id, name, symbol,
//! category, meme_delta, stance. `state.rs`, `voting-hieroglyph`, the sidecar,
//! kots-core and the frontend all derive from this table.
//!
//! ## Why one array of structs, and not a `match` per attribute
//!
//! Before this file existed the chain held THREE tables that disagreed inside
//! one binary: `state.rs` had a `names` array, `voting-hieroglyph/src/lib.rs`
//! had a `symbol_str` match and a separate `category_str` match. A single
//! `get_hieroglyph_info` response returned `name: "Love", symbol: "😂"` — the
//! name from one table, the emoji from another, describing different glyphs.
//! Off-chain there were two more: a DB seed (V002) and the frontend's own map.
//! Five tables, no two alike.
//!
//! One row per glyph makes that unrepresentable. The name and the emoji for id 3
//! sit on the same line; they cannot drift apart because there is nowhere for
//! them to drift TO.
//!
//! ## The rules this table encodes (HIEROGLYPH-REGISTRY.md)
//!
//! * **Cost is positional**: `fee = (id + 1) × 1 KOTS × (1 + gas)`, id 0 free.
//!   Cost is therefore NOT stored here — it is computed from the id. Storing it
//!   would be a second declaration of the same fact.
//! * **Stake rises with id**, so the ladder runs cheap expression → costly power.
//! * **`downvote` (2) is the only negative delta** on the platform (P4).
//! * **The alteration band (17–27) is the lowest positive at +6** (P2 — attacks
//!   are endorsements): defacing a meme still lifts it and can never bury it.
//! * **No glyph takes user input.** Every alteration is a pure function of
//!   `(master, glyph id, seed)`, so every render is derivable from chain data
//!   alone. `addtext`/`alttitle` were removed for breaking exactly that.
//! * **`is_active` does not exist.** A glyph is a paid action defined by the
//!   deployed program; disabling one off-chain while the chain still prices it
//!   would recreate the divergence this file eliminates.

use crate::constants::{BASE_KOTS_COST, DEFAULT_GAS_PRICE_LAMPORTS};

/// What a glyph DOES when it lands. Drives the off-chain dispatch: `Tally`
/// glyphs only score; `Alteration` glyphs additionally render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Function {
    /// Scores only. Changes nobody's view of the meme.
    Tally,
    /// Scores AND alters the rendered image for a time window (3h, §4b).
    Alteration,
}

/// Display grouping. Mirrors the bands the deltas are derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Basic,
    Reaction,
    Award,
    Alteration,
    Advanced,
}

impl Category {
    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Basic => "basic",
            Category::Reaction => "reaction",
            Category::Award => "award",
            Category::Alteration => "alteration",
            Category::Advanced => "advanced",
        }
    }
}

/// One glyph. One row. Every fact together.
#[derive(Clone, Copy, Debug)]
pub struct HieroglyphDef {
    pub id: u8,
    pub name: &'static str,
    pub symbol: &'static str,
    pub category: Category,
    pub function: Function,
    /// SIGNED meme-score delta. Permanent: the ledger records what was paid.
    pub meme_delta: i64,
    /// The prediction the payer takes on: +1 FOR, -1 AGAINST, 0 NEUTRAL.
    pub stance: i8,
}

use Category::{Advanced, Award, Basic, Reaction};
use Category::Alteration as AltCat;
use Function::{Alteration as Alter, Tally};

/// THE TABLE. 32 rows, ids 0..31, contiguous bands.
pub const HIEROGLYPHS: [HieroglyphDef; 32] = [
    // --- basic 0..2 — the core loop -----------------------------------------
    HieroglyphDef { id: 0,  name: "upvote",     symbol: "⬆",  category: Basic,    function: Tally, meme_delta: 10, stance: 1 },
    HieroglyphDef { id: 1,  name: "comment",    symbol: "💬", category: Basic,    function: Tally, meme_delta: 15, stance: 0 },
    HieroglyphDef { id: 2,  name: "downvote",   symbol: "⬇",  category: Basic,    function: Tally, meme_delta: -5, stance: -1 },
    // --- reaction 3..11 — emotes, flat +8: every one is the same act ---------
    HieroglyphDef { id: 3,  name: "laugh",      symbol: "😂", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 4,  name: "cry",        symbol: "😢", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 5,  name: "love",       symbol: "❤",  category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 6,  name: "bless",      symbol: "🙏", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 7,  name: "lucky",      symbol: "🍀", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 8,  name: "whoosh",     symbol: "💨", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 9,  name: "clap",       symbol: "👏", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 10, name: "foot",       symbol: "🦶", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    HieroglyphDef { id: 11, name: "brain",      symbol: "🧠", category: Reaction, function: Tally, meme_delta: 8,  stance: 1 },
    // --- award 12..16 — the medal ladder, delta escalates with cost ----------
    HieroglyphDef { id: 12, name: "wood",       symbol: "🪵", category: Award,    function: Tally, meme_delta: 12, stance: 1 },
    HieroglyphDef { id: 13, name: "silver",     symbol: "🥈", category: Award,    function: Tally, meme_delta: 14, stance: 1 },
    HieroglyphDef { id: 14, name: "gold",       symbol: "🥇", category: Award,    function: Tally, meme_delta: 16, stance: 1 },
    HieroglyphDef { id: 15, name: "platinum",   symbol: "💠", category: Award,    function: Tally, meme_delta: 18, stance: 1 },
    HieroglyphDef { id: 16, name: "diamond",    symbol: "💎", category: Award,    function: Tally, meme_delta: 20, stance: 1 },
    // --- alteration 17..27 — flat +6, the LOWEST positive (P2) ---------------
    // Every one renders. None takes user input. Sabotage cannot buy placement:
    // altering a meme lifts it and stakes you FOR it.
    HieroglyphDef { id: 17, name: "blackwhite", symbol: "⚫", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 18, name: "sketch",     symbol: "✏",  category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 19, name: "crop",       symbol: "✂",  category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 20, name: "steal",      symbol: "🏴", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 21, name: "flame",      symbol: "🔥", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 22, name: "rocket",     symbol: "🚀", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 23, name: "flip",       symbol: "🔄", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 24, name: "poster",     symbol: "🖼", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 25, name: "stamp",      symbol: "👍", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 26, name: "money",      symbol: "💰", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    HieroglyphDef { id: 27, name: "hide",       symbol: "👁", category: AltCat,   function: Alter, meme_delta: 6,  stance: 1 },
    // --- advanced 28..31 — apex tally, the maximum bets available ------------
    HieroglyphDef { id: 28, name: "frontpage",  symbol: "🏠", category: Advanced, function: Tally, meme_delta: 24, stance: 1 },
    HieroglyphDef { id: 29, name: "superheart", symbol: "💖", category: Advanced, function: Tally, meme_delta: 26, stance: 1 },
    HieroglyphDef { id: 30, name: "genius",     symbol: "🤓", category: Advanced, function: Tally, meme_delta: 28, stance: 1 },
    HieroglyphDef { id: 31, name: "crown",      symbol: "👑", category: Advanced, function: Tally, meme_delta: 30, stance: 1 },
];

/// The alteration band, as a closed range. Everything that needs "does this
/// glyph render?" asks here — off-chain included, via `get_hieroglyph_info`.
pub const ALTERATION_FIRST: u8 = 17;
pub const ALTERATION_LAST: u8 = 27;

/// Lookup. `None` for ids >= 32 — callers decide whether that is an error.
pub const fn get(id: u8) -> Option<&'static HieroglyphDef> {
    if (id as usize) < HIEROGLYPHS.len() {
        Some(&HIEROGLYPHS[id as usize])
    } else {
        None
    }
}

/// Signed meme-score delta. THE scoring accessor.
pub fn meme_delta(id: u32) -> i64 {
    match get_u32(id) {
        Some(d) => d.meme_delta,
        None => 0,
    }
}

/// Prediction stance: +1 FOR, -1 AGAINST, 0 NEUTRAL.
pub fn stance(id: u32) -> i8 {
    match get_u32(id) {
        Some(d) => d.stance,
        None => 0,
    }
}

/// Does this glyph alter the rendered image?
pub fn is_alteration(id: u32) -> bool {
    matches!(get_u32(id), Some(d) if matches!(d.function, Function::Alteration))
}

fn get_u32(id: u32) -> Option<&'static HieroglyphDef> {
    if id < 32 {
        get(id as u8)
    } else {
        None
    }
}

/// Positional cost, in lamport-scaled KOTS: `(id + 1) × BASE × (1 + gas)`,
/// id 0 free. Derived, never stored — the fee formula is the owner of this fact
/// (`KOTS_TOKENOMICS.md` §4a).
pub fn base_cost_lamports(id: u8) -> u64 {
    if id == 0 || id >= 32 {
        return 0;
    }
    let n = (id as u64) + 1;
    n * BASE_KOTS_COST + n * DEFAULT_GAS_PRICE_LAMPORTS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant that the five-table era violated: ids are dense, ordered,
    /// and every row describes itself.
    #[test]
    fn table_is_well_formed() {
        assert_eq!(HIEROGLYPHS.len(), 32);
        for (i, d) in HIEROGLYPHS.iter().enumerate() {
            assert_eq!(d.id as usize, i, "row {} carries id {}", i, d.id);
            assert!(!d.name.is_empty());
            assert!(!d.symbol.is_empty());
            // symbol must fit HieroglyphMetadata.symbol: [u8; 8]
            assert!(d.symbol.len() <= 8, "{} symbol is {} bytes", d.name, d.symbol.len());
            // name must fit InteractionType.name / HieroglyphMetadata.name: [u8; 32]
            assert!(d.name.len() <= 32);
            assert!(d.category.as_str().len() <= 24);
            assert!((-1..=1).contains(&d.stance));
        }
    }

    #[test]
    fn names_are_unique() {
        for i in 0..32 {
            for j in (i + 1)..32 {
                assert_ne!(HIEROGLYPHS[i].name, HIEROGLYPHS[j].name);
                assert_ne!(HIEROGLYPHS[i].symbol, HIEROGLYPHS[j].symbol);
            }
        }
    }

    /// P4: downvote is the ONLY negative on the platform.
    #[test]
    fn downvote_is_the_only_negative() {
        let neg: Vec<u8> = HIEROGLYPHS.iter().filter(|d| d.meme_delta < 0).map(|d| d.id).collect();
        assert_eq!(neg, vec![2]);
        assert_eq!(meme_delta(2), -5);
        assert_eq!(stance(2), -1);
    }

    /// P1: exactly one NEUTRAL (comment); everything else paid is FOR.
    #[test]
    fn stances_are_as_specified() {
        let neutral: Vec<u8> = HIEROGLYPHS.iter().filter(|d| d.stance == 0).map(|d| d.id).collect();
        assert_eq!(neutral, vec![1]);
        let against: Vec<u8> = HIEROGLYPHS.iter().filter(|d| d.stance == -1).map(|d| d.id).collect();
        assert_eq!(against, vec![2]);
    }

    /// P2: the alteration band is the lowest positive delta, so defacing a meme
    /// is the least efficient way to move it and can never bury it.
    #[test]
    fn alterations_are_the_lowest_positive() {
        for d in HIEROGLYPHS.iter().filter(|d| matches!(d.function, Function::Alteration)) {
            assert_eq!(d.meme_delta, 6, "{} must be +6", d.name);
        }
        for d in HIEROGLYPHS.iter().filter(|d| d.meme_delta > 0) {
            assert!(d.meme_delta >= 6, "{} is below the alteration floor", d.name);
        }
    }

    /// The alteration band is contiguous — off-chain code ranges over it.
    #[test]
    fn alteration_band_is_contiguous() {
        let band: Vec<u8> = HIEROGLYPHS.iter()
            .filter(|d| matches!(d.function, Function::Alteration))
            .map(|d| d.id).collect();
        assert_eq!(band, (ALTERATION_FIRST..=ALTERATION_LAST).collect::<Vec<u8>>());
        assert_eq!(band.len(), 11);
    }

    /// Cost is positional and derived, never stored. id 0 is free.
    ///
    /// PINNED TO THE OWNER, not to a copy of itself. This asserted
    /// `base_cost_lamports(id) == n * BASE_KOTS_COST + n * DEFAULT_GAS_PRICE_LAMPORTS`
    /// — the function's own body, retyped. It could never fail, whatever either side
    /// did, which is the same shape of nothing as a fixture that agrees with itself.
    ///
    /// The fee's owner is `utils::calculate_dynamic_hieroglyph_fee` (KOTS_TOKENOMICS.md
    /// §4a), and this helper is a second implementation of it. That duplication is
    /// exactly how voting-hieroglyph came to charge gas TWICE — the two sides drifted
    /// and nothing compared them. So compare them: if either moves, this fails.
    #[test]
    fn cost_is_positional_and_agrees_with_the_fee_formula() {
        assert_eq!(base_cost_lamports(0), 0, "id 0 is free");

        for id in 1..32u8 {
            let owner = crate::utils::calculate_dynamic_hieroglyph_fee(
                id,
                BASE_KOTS_COST,
                DEFAULT_GAS_PRICE_LAMPORTS,
            )
            .expect("the fee formula must answer for every id in range");

            assert_eq!(
                base_cost_lamports(id),
                owner,
                "glyph {id}: this helper and calculate_dynamic_hieroglyph_fee disagree — \
                 one of them is now charging the platform's users the wrong price"
            );
        }

        // ...and it still rises with the id, which is the point of a positional fee.
        assert!(base_cost_lamports(31) > base_cost_lamports(30));
    }

    #[test]
    fn out_of_range_is_inert() {
        assert!(get(32).is_none());
        assert_eq!(meme_delta(32), 0);
        assert_eq!(stance(99), 0);
        assert!(!is_alteration(32));
    }
}
