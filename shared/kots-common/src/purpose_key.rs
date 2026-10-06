// kots-contracts/shared/kots-common/src/purpose_key.rs
//! ACT-24 — the transfer `purpose` as a STRUCTURED accounting key (F15).
//!
//! One canonical, human-readable string per bucket, machine-parsed by the
//! contract to select the bucket's ledger — the same bytes an owner reads in
//! a watcher alert or the digest. Grammar (ASCII, strict):
//!
//! ```text
//! contest-award:<u64>          winner awards of contest N
//! contest-voter-share:<u64>    voter shares of contest N
//! jackpot                      THE jackpot pool (id-less; its own bound)
//! treasury-op:<u64>            operational batch N (ACT-25's tiers)
//! tier-stake:<u64>             tier-qualification stakes, tier N
//! ```
//!
//! Strictness is deliberate: exactly one byte-string names each bucket (no
//! leading zeros, no spaces, no case variance), so audit logs, alerts, and
//! ledger PDAs can never disagree about which bucket a transfer debited.
//!
//! Dependency-free ON PURPOSE (core + alloc only): the module runs under
//! `rustc --test` standalone — which is how it is verified in an offline
//! sandbox — and the same bytes-level grammar is re-implemented by the
//! sidecar/watcher TypeScript with these tests as the shared contract.

pub const TAG_CONTEST_AWARD: u8 = 0;
pub const TAG_CONTEST_VOTER_SHARE: u8 = 1;
pub const TAG_JACKPOT: u8 = 2;
pub const TAG_TREASURY_OP: u8 = 3;
pub const TAG_TIER_STAKE: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurposeKey {
    ContestAward { contest_id: u64 },
    ContestVoterShare { contest_id: u64 },
    Jackpot,
    TreasuryOp { op_id: u64 },
    TierStake { tier_id: u64 },
}

impl PurposeKey {
    /// Strict parse of the canonical grammar; anything else is None.
    pub fn parse(bytes: &[u8]) -> Option<PurposeKey> {
        if bytes == b"jackpot" {
            return Some(PurposeKey::Jackpot);
        }
        let prefixes: [(&[u8], fn(u64) -> PurposeKey); 4] = [
            (b"contest-award:", |id| PurposeKey::ContestAward { contest_id: id }),
            (b"contest-voter-share:", |id| PurposeKey::ContestVoterShare { contest_id: id }),
            (b"treasury-op:", |id| PurposeKey::TreasuryOp { op_id: id }),
            (b"tier-stake:", |id| PurposeKey::TierStake { tier_id: id }),
        ];
        for (prefix, build) in prefixes {
            if let Some(rest) = bytes.strip_prefix(prefix) {
                return parse_canonical_u64(rest).map(build);
            }
        }
        None
    }

    pub fn tag(&self) -> u8 {
        match self {
            PurposeKey::ContestAward { .. } => TAG_CONTEST_AWARD,
            PurposeKey::ContestVoterShare { .. } => TAG_CONTEST_VOTER_SHARE,
            PurposeKey::Jackpot => TAG_JACKPOT,
            PurposeKey::TreasuryOp { .. } => TAG_TREASURY_OP,
            PurposeKey::TierStake { .. } => TAG_TIER_STAKE,
        }
    }

    /// The ledger id under the tag; the id-less jackpot is bucket 0 of tag 2.
    pub fn bucket_id(&self) -> u64 {
        match self {
            PurposeKey::ContestAward { contest_id } => *contest_id,
            PurposeKey::ContestVoterShare { contest_id } => *contest_id,
            PurposeKey::Jackpot => 0,
            PurposeKey::TreasuryOp { op_id } => *op_id,
            PurposeKey::TierStake { tier_id } => *tier_id,
        }
    }

    /// The one canonical byte-string; `parse(render(k)) == Some(k)` always.
    pub fn render(&self) -> alloc::string::String {
        use alloc::format;
        match self {
            PurposeKey::ContestAward { contest_id } => format!("contest-award:{contest_id}"),
            PurposeKey::ContestVoterShare { contest_id } => {
                format!("contest-voter-share:{contest_id}")
            }
            PurposeKey::Jackpot => alloc::string::String::from("jackpot"),
            PurposeKey::TreasuryOp { op_id } => format!("treasury-op:{op_id}"),
            PurposeKey::TierStake { tier_id } => format!("tier-stake:{tier_id}"),
        }
    }
}

extern crate alloc;

/// Canonical u64: 1..=20 ASCII digits, no leading zero unless exactly "0",
/// checked against u64::MAX. One byte-string per number, per bucket.
fn parse_canonical_u64(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() || digits.len() > 20 {
        return None;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return None;
    }
    let mut value: u64 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(d - b'0'))?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_variant() {
        let keys = [
            PurposeKey::ContestAward { contest_id: 7 },
            PurposeKey::ContestVoterShare { contest_id: 42 },
            PurposeKey::Jackpot,
            PurposeKey::TreasuryOp { op_id: 0 },
            PurposeKey::TierStake { tier_id: u64::MAX },
        ];
        for k in keys {
            assert_eq!(PurposeKey::parse(k.render().as_bytes()), Some(k), "{:?}", k);
        }
    }

    #[test]
    fn tags_and_bucket_ids_are_stable() {
        assert_eq!(PurposeKey::ContestAward { contest_id: 9 }.tag(), TAG_CONTEST_AWARD);
        assert_eq!(PurposeKey::ContestVoterShare { contest_id: 9 }.tag(), TAG_CONTEST_VOTER_SHARE);
        assert_eq!(PurposeKey::Jackpot.tag(), TAG_JACKPOT);
        assert_eq!(PurposeKey::Jackpot.bucket_id(), 0);
        assert_eq!(PurposeKey::TreasuryOp { op_id: 3 }.bucket_id(), 3);
        assert_eq!(PurposeKey::TierStake { tier_id: 4 }.tag(), TAG_TIER_STAKE);
    }

    #[test]
    fn rejects_everything_non_canonical() {
        let bad: &[&[u8]] = &[
            b"",
            b"free text ops note",
            b"contest-award:",          // missing id
            b"contest-award:07",        // leading zero — two names, one bucket: refused
            b"contest-award:7 ",        // trailing junk
            b" contest-award:7",        // leading junk
            b"Contest-Award:7",         // case variance
            b"contest-award:18446744073709551616", // u64::MAX + 1
            b"contest-award:123456789012345678901", // 21 digits
            b"jackpot:1",               // jackpot is id-less
            b"jackpot ",
            b"treasury-op:-1",
            b"tier-stake:1.5",
        ];
        for b in bad {
            assert_eq!(
                PurposeKey::parse(b),
                None,
                "must reject {:?}",
                core::str::from_utf8(b)
            );
        }
    }

    #[test]
    fn accepts_the_canonical_edges() {
        assert_eq!(
            PurposeKey::parse(b"treasury-op:0"),
            Some(PurposeKey::TreasuryOp { op_id: 0 })
        );
        assert_eq!(
            PurposeKey::parse(b"tier-stake:18446744073709551615"),
            Some(PurposeKey::TierStake { tier_id: u64::MAX })
        );
    }
}
