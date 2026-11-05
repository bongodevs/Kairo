//! Strategy-specific IP address selection and rotation logic.

use std::net::IpAddr;
use std::sync::atomic::Ordering;

use rand::Rng;

use super::ip_block::BLOCK64_IPS;
use super::IpRoutePlanner;
use crate::config::RatelimitStrategy;
use crate::utils::locked;

// The two nano strategies, which need an IPv6 `/64` or wider.
pub(super) fn is_nano(strategy: RatelimitStrategy) -> bool {
    matches!(
        strategy,
        RatelimitStrategy::NanoSwitch | RatelimitStrategy::RotatingNanoSwitch
    )
}

impl IpRoutePlanner {
    // The `RotateOnBan` address. The pick is sticky until a ban sets `rotate_pending`; a fresh pick
    // jumps a random 10..19 addresses on a block wider than 128 so a ban does not land on the
    // neighbouring, most likely equally rate-limited address, then scans forward from there.
    pub(super) fn rotate_on_ban_address(&self) -> IpAddr {
        // `current` before `failing` (through `is_failing`), the lock order used everywhere here.
        let mut current = locked(&self.current);
        if !self.rotate_pending.swap(false, Ordering::AcqRel) {
            if let Some(address) = *current {
                return address;
            }
        }
        let step: u128 = if self.total_size > 128 {
            rand::thread_rng().gen_range(10..20)
        } else {
            1
        };
        let index = (self.ip_index.load(Ordering::Acquire) as u128).wrapping_add(step);
        // `ip_index` counts addresses handed out and the pick sits one below it, so the first pick
        // is the block's base rather than the address after it.
        let (address, offset) = self.usable_from(index.wrapping_sub(1));
        self.ip_index
            .store(offset.wrapping_add(1) as u64, Ordering::Release);
        *current = Some(address);
        address
    }

    // Nanoseconds elapsed since the current `/64` was entered.
    pub(super) fn nanos_in_block(&self) -> u128 {
        let elapsed = self.created.elapsed().as_nanos();
        elapsed.saturating_sub(self.block_start_nanos.load(Ordering::Acquire) as u128)
    }

    // The nano strategies index by the nanosecond clock itself, so consecutive requests land on
    // consecutive addresses.
    pub(super) fn nano_index(&self) -> u128 {
        let offset = self.nanos_in_block();
        if self.strategy == RatelimitStrategy::RotatingNanoSwitch {
            let block = self.nano_block.load(Ordering::Acquire) as u128;
            return block.wrapping_mul(BLOCK64_IPS).wrapping_add(offset);
        }
        // The clock only spans one `/64`, so a wider block picks one at random per request.
        let blocks64 = self.total_size / BLOCK64_IPS;
        if blocks64 > 1 {
            let chosen = rand::thread_rng().gen_range(0..blocks64);
            return chosen.wrapping_mul(BLOCK64_IPS).wrapping_add(offset);
        }
        offset
    }
}
