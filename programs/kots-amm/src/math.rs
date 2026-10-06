// DESTINATION: kots-contracts/programs/kots-amm/src/math.rs
//! On-program copy of the P0 arithmetic in `docs/AMM-SPEC.md` §3.
//! Keep identical to `kots-contracts/amm-math`.

pub const BPS_DENOM: u128 = 10_000;
pub const T22_BPS: u64 = 300;
pub const AMM_LEG_BPS: u64 = 10;

/// ExtraAccountMetaList PDA + 7 live list entries (2026-07-29 O32 cut).
/// Prepare may pass more. Never fewer. Same constant as hook::HOOK_REMAINING_MIN.
pub const MIN_HOOK_EXTRAS: usize = 8;

pub fn apply_bps_floor(amount: u64, bps: u64) -> Option<u64> {
    let v = (amount as u128)
        .checked_mul(bps as u128)?
        .checked_div(BPS_DENOM)?;
    u64::try_from(v).ok()
}

pub fn t22_fee(amount: u64, bps: u64) -> Option<u64> {
    if bps == 0 || amount == 0 {
        return Some(0);
    }
    let numerator = (amount as u128).checked_mul(bps as u128)?;
    let raw = numerator / BPS_DENOM;
    let fee = if numerator % BPS_DENOM > 0 { raw + 1 } else { raw };
    u64::try_from(core::cmp::min(fee, amount as u128)).ok()
}

pub fn net_received(amount: u64, bps: u64) -> Option<u64> {
    amount.checked_sub(t22_fee(amount, bps)?)
}

pub fn amm_sol_split(sol: u64) -> Option<(u64, u64, u64)> {
    let captain = apply_bps_floor(sol, AMM_LEG_BPS)?;
    let bounty = apply_bps_floor(sol, AMM_LEG_BPS)?;
    let rest = sol.checked_sub(captain)?.checked_sub(bounty)?;
    Some((captain, bounty, rest))
}

pub fn cp_out(reserve_in: u64, reserve_out: u64, amount_in: u64) -> Option<u64> {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return None;
    }
    let x = reserve_in as u128;
    let y = reserve_out as u128;
    let dx = amount_in as u128;
    let k = x.checked_mul(y)?;
    let new_in = x.checked_add(dx)?;
    let new_out = k.checked_div(new_in)?;
    let out = y.checked_sub(new_out)?;
    u64::try_from(out).ok()
}

/// Sell quote after observing `landed` tradable KOTS (vault post − vault pre).
/// `user_sol` is what `min_out` is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellQuote {
    pub landed: u64,
    pub sol_out: u64,
    pub captain: u64,
    pub bounty: u64,
    pub user_sol: u64,
}

pub fn quote_sell(reserve_kots: u64, reserve_wsol: u64, landed: u64) -> Option<SellQuote> {
    let sol_out = cp_out(reserve_kots, reserve_wsol, landed)?;
    let (captain, bounty, user_sol) = amm_sol_split(sol_out)?;
    Some(SellQuote {
        landed,
        sol_out,
        captain,
        bounty,
        user_sol,
    })
}

/// Buy quote. `user_kots` is post-withhold — what `min_out` is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuyQuote {
    pub captain: u64,
    pub bounty: u64,
    pub into_k: u64,
    pub kots_out: u64,
    pub t22_fee: u64,
    pub user_kots: u64,
}

pub fn quote_buy(reserve_kots: u64, reserve_wsol: u64, sol_in: u64) -> Option<BuyQuote> {
    let (captain, bounty, into_k) = amm_sol_split(sol_in)?;
    let kots_out = cp_out(reserve_wsol, reserve_kots, into_k)?;
    let fee = t22_fee(kots_out, T22_BPS)?;
    let user_kots = kots_out.checked_sub(fee)?;
    Some(BuyQuote {
        captain,
        bounty,
        into_k,
        kots_out,
        t22_fee: fee,
        user_kots,
    })
}
