//! IP rotation for outbound HTTP requests, configured under `lavalink.server.ratelimit`.
//!
//! Each strategy hands the HTTP stack a source address to bind, and an address that comes back
//! rate-limited is marked failing and skipped until it expires.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{TimeZone, Utc};
use rand::Rng;

use player::tools::http_config::RoutePlanner;

use crate::config::{RatelimitConfig, RatelimitStrategy};
use crate::protocol::routeplanner::{
    BalancingIpDetails, FailingAddress, IpBlockStatus, NanoIpDetails, RotatingIpDetails,
    RotatingNanoIpDetails, RoutePlannerStatus,
};
use crate::utils::locked;
use crate::utils::time::now_millis;

mod ip_block;
mod strategy;
#[cfg(test)]
mod tests;

use ip_block::{IpBlock, BLOCK64_IPS};
use strategy::is_nano;

// How many addresses to scan past excluded or failing ones before giving up.
const MAX_SCAN: u128 = 1024;

// How long an address stays marked failing before it is retried anyway. Without this the pool only
// ever shrinks.
const FAILING_TIME_MILLIS: i64 = 7 * 24 * 60 * 60 * 1000;

/// An IP route planner over one or more CIDR blocks.
pub struct IpRoutePlanner {
    blocks: Vec<IpBlock>,
    pub(super) total_size: u128,
    pub(super) excluded: Vec<IpAddr>,
    pub(super) strategy: RatelimitStrategy,
    // How many times a ban has moved the planner off an address, reported as `rotateIndex`.
    pub(super) rotate_index: AtomicU64,
    // One past the offset `RotateOnBan` currently sits on, reported as `ipIndex`.
    pub(super) ip_index: AtomicU64,
    // Set by a ban so the next `RotateOnBan` request picks again instead of reusing `current`.
    pub(super) rotate_pending: AtomicBool,
    // Origin of the nano strategies' clock.
    pub(super) created: Instant,
    // Which `/64` `RotatingNanoSwitch` is in, advanced on a ban.
    pub(super) nano_block: AtomicU64,
    // Nanoseconds since `created` at which that `/64` was entered.
    pub(super) block_start_nanos: AtomicU64,
    // Addresses currently marked failing, each with the unix-ms timestamp of the failure.
    pub(super) failing: Mutex<HashMap<IpAddr, i64>>,
    // The most recently handed-out address, for the status payload and `RotateOnBan`'s sticky pick.
    pub(super) current: Mutex<Option<IpAddr>>,
    // The address the last ban was for, so repeated bans of one address rotate once.
    pub(super) last_failing: Mutex<Option<IpAddr>>,
}

impl IpRoutePlanner {
    /// Build a planner from `lavalink.server.ratelimit`, or `None` when it is off or no block
    /// parses. Every block that fails to parse is logged.
    pub fn from_config(config: &RatelimitConfig) -> Option<Arc<Self>> {
        if !config.is_enabled() {
            return None;
        }
        let mut blocks = Vec::new();
        for cidr in &config.ip_blocks {
            match IpBlock::parse(cidr) {
                Some(block) => blocks.push(block),
                None => tracing::warn!(block = %cidr, "Ignoring invalid ratelimit ipBlock"),
            }
        }
        if blocks.is_empty() {
            tracing::warn!(
                "ratelimit.ipBlocks contained no valid CIDR blocks; route planner disabled"
            );
            return None;
        }
        let total_size = blocks
            .iter()
            .fold(0u128, |acc, b| acc.saturating_add(b.size));
        // Both nano strategies index a `/64` with a 64-bit nanosecond clock, so an IPv4 or smaller
        // pool has nothing to rotate through.
        if is_nano(config.strategy) && (!blocks[0].is_ipv6 || total_size < BLOCK64_IPS) {
            tracing::warn!(
                strategy = ?config.strategy,
                "ratelimit.strategy needs an IPv6 block of at least a /64; route planner disabled"
            );
            return None;
        }
        let excluded = config
            .excluded_ips
            .iter()
            .filter_map(|ip| match ip.trim().parse::<IpAddr>() {
                Ok(address) => Some(address),
                Err(_) => {
                    tracing::warn!(address = %ip, "Ignoring unparseable ratelimit excludedIp");
                    None
                }
            })
            .collect();

        Some(Arc::new(Self {
            blocks,
            total_size,
            excluded,
            strategy: config.strategy,
            rotate_index: AtomicU64::new(0),
            ip_index: AtomicU64::new(0),
            rotate_pending: AtomicBool::new(false),
            created: Instant::now(),
            nano_block: AtomicU64::new(0),
            block_start_nanos: AtomicU64::new(0),
            failing: Mutex::new(HashMap::new()),
            current: Mutex::new(None),
            last_failing: Mutex::new(None),
        }))
    }

    // The address at a global index spanning every block, wrapping by `total_size`.
    fn address_at_global(&self, index: u128) -> IpAddr {
        let mut offset = index % self.total_size;
        for block in &self.blocks {
            if offset < block.size {
                return block.address_at(offset);
            }
            offset -= block.size;
        }
        // Unreachable while `offset < total_size`, but fall back to the first block's base.
        self.blocks[0].address_at(0)
    }

    fn is_excluded(&self, address: &IpAddr) -> bool {
        self.excluded.contains(address)
    }

    fn is_failing(&self, address: &IpAddr) -> bool {
        let mut failing = locked(&self.failing);
        match failing.get(address) {
            // An expired entry is dropped and the address retried. Only the address asked about:
            // this runs up to `MAX_SCAN` times per request, so walking the whole map here would
            // make every scan step O(n).
            Some(&at) if at < expiry_cutoff() => {
                failing.remove(address);
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    // The first usable address at or after `start`, with the global offset it was found at so
    // `RotateOnBan` can resume from there.
    pub(super) fn usable_from(&self, start: u128) -> (IpAddr, u128) {
        let scan = self.total_size.min(MAX_SCAN);
        for i in 0..scan {
            let offset = start.wrapping_add(i) % self.total_size;
            let address = self.address_at_global(offset);
            if !self.is_excluded(&address) && !self.is_failing(&address) {
                return (address, offset);
            }
        }
        // Everything in range is excluded or failing, so hand back the start address anyway.
        let offset = start % self.total_size;
        (self.address_at_global(offset), offset)
    }

    /// The payload behind `GET /v4/routeplanner/status`.
    pub fn status(&self) -> RoutePlannerStatus {
        // Every configured block is reported as one combined block: the size is the total and the
        // type comes from the first.
        let ip_block = IpBlockStatus {
            block_type: self.blocks[0].type_name().to_string(),
            size: self.total_size.to_string(),
        };
        let failing_addresses = self.failing_addresses();

        match self.strategy {
            RatelimitStrategy::RotateOnBan => {
                let current = locked(&self.current)
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                RoutePlannerStatus::RotatingIp(RotatingIpDetails {
                    ip_block,
                    failing_addresses,
                    rotate_index: self.rotate_index.load(Ordering::Acquire).to_string(),
                    ip_index: self.ip_index.load(Ordering::Acquire).to_string(),
                    current_address: current,
                })
            }
            RatelimitStrategy::NanoSwitch => RoutePlannerStatus::NanoIp(NanoIpDetails {
                ip_block,
                failing_addresses,
                current_address_index: self.nanos_in_block().to_string(),
            }),
            RatelimitStrategy::RotatingNanoSwitch => {
                RoutePlannerStatus::RotatingNanoIp(RotatingNanoIpDetails {
                    ip_block,
                    failing_addresses,
                    block_index: self.nano_block.load(Ordering::Acquire).to_string(),
                    current_address_index: self.nanos_in_block().to_string(),
                })
            }
            RatelimitStrategy::LoadBalance => RoutePlannerStatus::BalancingIp(BalancingIpDetails {
                ip_block,
                failing_addresses,
            }),
        }
    }

    fn failing_addresses(&self) -> Vec<FailingAddress> {
        let mut failing = locked(&self.failing);
        prune_expired(&mut failing);
        failing
            .iter()
            .map(|(address, &timestamp)| FailingAddress {
                failing_address: address.to_string(),
                failing_timestamp: timestamp,
                failing_time: format_millis(timestamp),
            })
            .collect()
    }

    /// Stop treating `address` as failing. `true` if it was.
    pub fn free_address(&self, address: &IpAddr) -> bool {
        locked(&self.failing).remove(address).is_some()
    }

    /// Stop treating every address as failing.
    pub fn free_all(&self) {
        locked(&self.failing).clear();
    }
}

impl RoutePlanner for IpRoutePlanner {
    fn next_address(&self) -> Option<IpAddr> {
        if self.total_size == 0 {
            return None;
        }
        let address = match self.strategy {
            // Sticky: the same address until it is banned.
            RatelimitStrategy::RotateOnBan => return Some(self.rotate_on_ban_address()),
            // The clock is the index, so every request gets a fresh address on its own.
            RatelimitStrategy::NanoSwitch | RatelimitStrategy::RotatingNanoSwitch => {
                self.usable_from(self.nano_index()).0
            }
            // A random address per request, skipping excluded and failing ones.
            RatelimitStrategy::LoadBalance => {
                let mut rng = rand::thread_rng();
                let mut chosen = None;
                for _ in 0..16 {
                    let address = self.address_at_global(rng.gen_range(0..self.total_size));
                    if !self.is_excluded(&address) && !self.is_failing(&address) {
                        chosen = Some(address);
                        break;
                    }
                }
                chosen.unwrap_or_else(|| self.address_at_global(rng.gen_range(0..self.total_size)))
            }
        };
        *locked(&self.current) = Some(address);
        Some(address)
    }

    fn mark_failing(&self, address: IpAddr) {
        {
            let mut failing = locked(&self.failing);
            // Prune here rather than only when the status endpoint is polled: an unpolled node on a
            // large block would otherwise keep an entry per banned address forever, and a ban is
            // the only moment the map grows.
            prune_expired(&mut failing);
            failing.insert(address, now_millis());
        }
        tracing::warn!(%address, "Route planner marked source address as failing (rate-limited)");
        // A rate-limited address usually fails on several in-flight requests at once, and those
        // duplicates must not each cost a rotation step or the block drains N addresses per ban.
        let repeat = {
            let mut last = locked(&self.last_failing);
            let repeat = *last == Some(address);
            *last = Some(address);
            repeat
        };
        if repeat {
            return;
        }
        match self.strategy {
            // Step past the banned address on the next request.
            RatelimitStrategy::RotateOnBan => {
                self.rotate_index.fetch_add(1, Ordering::AcqRel);
                self.rotate_pending.store(true, Ordering::Release);
            }
            // Abandon the whole `/64` and restart the clock inside the next one.
            RatelimitStrategy::RotatingNanoSwitch => {
                self.nano_block.fetch_add(1, Ordering::AcqRel);
                self.block_start_nanos
                    .store(self.created.elapsed().as_nanos() as u64, Ordering::Release);
            }
            // Nothing to advance, the next request picks a different address regardless.
            RatelimitStrategy::NanoSwitch | RatelimitStrategy::LoadBalance => {}
        }
    }
}

// Unix-ms before which a recorded failure has expired and the address is usable again.
fn expiry_cutoff() -> i64 {
    now_millis().saturating_sub(FAILING_TIME_MILLIS)
}

fn prune_expired(failing: &mut HashMap<IpAddr, i64>) {
    let cutoff = expiry_cutoff();
    failing.retain(|_, at| *at >= cutoff);
}

// The human-readable form of a timestamp, as `failingTime` reports it.
fn format_millis(millis: i64) -> String {
    match Utc.timestamp_millis_opt(millis).single() {
        Some(dt) => dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
        None => millis.to_string(),
    }
}
