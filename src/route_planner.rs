//! The v4 route planner, following the reference's `youtube-rotator` planners.
//!
//! The reference builds the planner only for its bundled YouTube source. Kairo has no such source,
//! so the planner exists for the REST contract (`status`, `free/address`, `free/all`) and to bind
//! the generic HTTP source's outbound address to the planner's current one. Nothing in scope marks
//! an address as failing, so `failingAddresses` stays empty until a rotating provider is added; the
//! free endpoints still operate on that state.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, bail};
use serde::Serialize;

use crate::config::RateLimitConfig;
use crate::utils::locked;
use crate::utils::time::now_millis;

/// One /64 in addresses, the reference's `Ipv6Block.BLOCK64_IPS`.
const BLOCK64: u128 = 1 << 64;
/// How long a marked address stays out of rotation, the reference's `FAILING_TIME`.
const FAILING_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    V4,
    V6,
}

impl Family {
    const fn bits(self) -> u32 {
        match self {
            Self::V4 => 32,
            Self::V6 => 128,
        }
    }

    const fn type_name(self) -> &'static str {
        match self {
            Self::V4 => "Inet4Address",
            Self::V6 => "Inet6Address",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Cidr {
    family: Family,
    prefix: u128,
    mask_bits: u8,
}

impl Cidr {
    fn parse(text: &str) -> anyhow::Result<Self> {
        let text = text.trim();
        let (address, mask) = match text.split_once('/') {
            Some((address, mask)) => (address.trim(), Some(mask.trim())),
            None => (text, None),
        };
        if let Ok(address) = Ipv4Addr::from_str(address) {
            return Ok(Self {
                family: Family::V4,
                prefix: u128::from(u32::from(address)),
                mask_bits: parse_mask(mask, 32)?,
            });
        }
        let address =
            Ipv6Addr::from_str(address).with_context(|| format!("invalid CIDR block '{text}'"))?;
        let mask_bits = parse_mask(mask, 128)?;
        let mask = if mask_bits == 0 { 0 } else { u128::MAX << (128 - u32::from(mask_bits)) };
        Ok(Self { family: Family::V6, prefix: u128::from(address) & mask, mask_bits })
    }

    const fn size_bits(self) -> u32 {
        self.family.bits() - self.mask_bits as u32
    }

    /// `None` when the block is a whole IPv6 space, which does not fit in `u128`.
    fn size(self) -> Option<u128> {
        let bits = self.size_bits();
        (bits < 128).then(|| 1_u128 << bits)
    }

    fn address_at(self, index: u128) -> Option<IpAddr> {
        if self.size().is_some_and(|size| index >= size) {
            return None;
        }
        Some(match self.family {
            Family::V4 => {
                IpAddr::V4(Ipv4Addr::from((self.prefix as u32).wrapping_add(index as u32)))
            }
            Family::V6 => IpAddr::V6(Ipv6Addr::from(self.prefix.wrapping_add(index).to_be_bytes())),
        })
    }
}

fn parse_mask(mask: Option<&str>, max: u8) -> anyhow::Result<u8> {
    let Some(mask) = mask else {
        return Ok(max);
    };
    let value: u8 = mask.parse().with_context(|| format!("invalid CIDR mask '{mask}'"))?;
    if value > max {
        bail!("invalid CIDR mask '{mask}'");
    }
    Ok(value)
}

/// One or more CIDR blocks of the same family, treated as a single virtual block like the
/// reference's `CombinedIpBlock`.
#[derive(Debug, Clone)]
struct IpBlock {
    blocks: Vec<Cidr>,
    family: Family,
}

impl IpBlock {
    fn parse_all(values: &[String]) -> anyhow::Result<Self> {
        let Some(first) = values.first() else {
            bail!("no IP blocks configured");
        };
        let first = Cidr::parse(first).with_context(|| format!("invalid IP block '{first}'"))?;
        let mut blocks = vec![first];
        for value in &values[1..] {
            let block =
                Cidr::parse(value).with_context(|| format!("invalid IP block '{value}'"))?;
            if block.family != first.family {
                bail!("all IP blocks must have the same address family");
            }
            blocks.push(block);
        }
        Ok(Self { blocks, family: first.family })
    }

    fn size(&self) -> Option<u128> {
        self.blocks.iter().try_fold(0_u128, |sum, block| sum.checked_add(block.size()?))
    }

    fn at_least_block64(&self) -> bool {
        self.size().is_none_or(|size| size >= BLOCK64)
    }

    fn address_at(&self, index: u128) -> Option<IpAddr> {
        if let [block] = self.blocks.as_slice() {
            return block.address_at(index);
        }
        let mut index = index;
        for block in &self.blocks {
            let size = block.size()?;
            if index < size {
                return block.address_at(index);
            }
            index -= size;
        }
        None
    }

    fn size_decimal(&self) -> String {
        self.blocks.iter().fold(String::from("0"), |sum, block| {
            add_decimal(&sum, &pow2_decimal(block.size_bits()))
        })
    }

    const fn type_name(&self) -> &'static str {
        self.family.type_name()
    }
}

fn pow2_decimal(bits: u32) -> String {
    let mut digits = vec![1_u8];
    for _ in 0..bits {
        double(&mut digits);
    }
    decimal(digits)
}

fn double(digits: &mut Vec<u8>) {
    let mut carry = 0;
    for digit in digits.iter_mut() {
        let value = *digit * 2 + carry;
        *digit = value % 10;
        carry = value / 10;
    }
    if carry > 0 {
        digits.push(carry);
    }
}

fn add_decimal(left: &str, right: &str) -> String {
    let mut digits = Vec::new();
    let mut left = left.bytes().rev();
    let mut right = right.bytes().rev();
    let mut carry = 0;
    loop {
        let a = left.next().map(|digit| digit - b'0');
        let b = right.next().map(|digit| digit - b'0');
        if a.is_none() && b.is_none() && carry == 0 {
            break;
        }
        let value = a.unwrap_or(0) + b.unwrap_or(0) + carry;
        digits.push(value % 10);
        carry = value / 10;
    }
    if digits.is_empty() {
        digits.push(0);
    }
    decimal(digits)
}

fn decimal(mut digits: Vec<u8>) -> String {
    if digits.is_empty() {
        digits.push(0);
    }
    digits.iter().rev().map(|digit| char::from(b'0' + digit)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    RotateOnBan,
    LoadBalance,
    NanoSwitch,
    RotatingNanoSwitch,
}

impl Strategy {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rotateonban" => Ok(Self::RotateOnBan),
            "loadbalance" => Ok(Self::LoadBalance),
            "nanoswitch" => Ok(Self::NanoSwitch),
            "rotatingnanoswitch" => Ok(Self::RotatingNanoSwitch),
            _ => bail!("Unknown strategy!"),
        }
    }

    const fn class(self) -> &'static str {
        match self {
            Self::RotateOnBan => "RotatingIpRoutePlanner",
            Self::LoadBalance => "BalancingIpRoutePlanner",
            Self::NanoSwitch => "NanoIpRoutePlanner",
            Self::RotatingNanoSwitch => "RotatingNanoIpRoutePlanner",
        }
    }
}

pub(crate) struct RoutePlanner {
    strategy: Strategy,
    block: IpBlock,
    excluded: Vec<IpAddr>,
    failing: Mutex<BTreeMap<IpAddr, i64>>,
    /// `RotatingIpRoutePlanner`: one-based index of the chosen address, zero before the first pick.
    index: AtomicU64,
    rotate_index: AtomicU64,
    /// Set when the chosen address failed, so the next request asks for a new one.
    next: AtomicBool,
    nano_start: Instant,
    /// `RotatingNanoIpRoutePlanner`: the current /64 block and when it started, as nanoseconds
    /// since the planner was built.
    block_index: AtomicU64,
    block_start: AtomicU64,
}

impl RoutePlanner {
    /// Build the planner from `[lavalink.server.ratelimit]`, or nothing when the block is absent or
    /// lists no blocks, exactly like the reference's bean.
    pub(crate) fn build(config: Option<&RateLimitConfig>) -> anyhow::Result<Option<Self>> {
        let Some(config) = config else {
            return Ok(None);
        };
        if config.ip_blocks.is_empty() {
            tracing::info!("list of IP blocks is empty, skipping route planner setup");
            return Ok(None);
        }
        let strategy = Strategy::parse(&config.strategy)?;
        let block = IpBlock::parse_all(&config.ip_blocks)?;
        let needs_block64 = matches!(strategy, Strategy::NanoSwitch | Strategy::RotatingNanoSwitch);
        if needs_block64 && (block.family != Family::V6 || !block.at_least_block64()) {
            bail!("the {} strategy requires an IPv6 block of at least /64", strategy.class());
        }
        let mut excluded = Vec::with_capacity(config.excluded_ips.len());
        for value in &config.excluded_ips {
            let address = IpAddr::from_str(value.trim())
                .with_context(|| format!("invalid excluded IP '{value}'"))?;
            excluded.push(address);
        }
        let now = Instant::now();
        let planner = Self {
            strategy,
            block,
            excluded,
            failing: Mutex::new(BTreeMap::new()),
            index: AtomicU64::new(0),
            rotate_index: AtomicU64::new(0),
            next: AtomicBool::new(false),
            nano_start: now,
            block_index: AtomicU64::new(0),
            block_start: AtomicU64::new(0),
        };
        tracing::info!(
            class = strategy.class(),
            ips = %planner.block.size_decimal(),
            "active route planner"
        );
        if config.retry_limit != -1 || !config.search_triggers_fail {
            tracing::warn!(
                "retryLimit and searchTriggersFail only affect the reference's bundled YouTube \
                 source; no configured Kairo source marks bans, so they have no effect"
            );
        }
        Ok(Some(planner))
    }

    /// The local address the next outbound request should bind to.
    pub(crate) fn current_address(&self) -> Option<IpAddr> {
        match self.strategy {
            Strategy::RotateOnBan => self.rotating_address(),
            Strategy::LoadBalance => self.balancing_address(),
            Strategy::NanoSwitch => self.nano_address(),
            Strategy::RotatingNanoSwitch => self.rotating_nano_address(),
        }
    }

    pub(crate) fn status(&self) -> Status {
        let ip_block =
            BlockStatus { kind: self.block.type_name(), size: self.block.size_decimal() };
        let details = match self.strategy {
            Strategy::RotateOnBan => {
                let index = self.index.load(Ordering::Acquire);
                let current = match index.checked_sub(1) {
                    Some(index) => self.block.address_at(u128::from(index)),
                    None => None,
                };
                Details::RotatingIp(RotatingIpDetails {
                    ip_block,
                    failing_addresses: self.failing_addresses(),
                    rotate_index: self.rotate_index.load(Ordering::Acquire).to_string(),
                    ip_index: index.to_string(),
                    current_address: current
                        .map_or_else(|| String::from("null"), |a| a.to_string()),
                })
            }
            Strategy::LoadBalance => Details::Balancing(BalancingIpDetails {
                ip_block,
                failing_addresses: self.failing_addresses(),
            }),
            Strategy::NanoSwitch => Details::Nano(NanoIpDetails {
                ip_block,
                failing_addresses: self.failing_addresses(),
                current_address_index: self.nano_offset().to_string(),
            }),
            Strategy::RotatingNanoSwitch => Details::RotatingNano(RotatingNanoDetails {
                ip_block,
                failing_addresses: self.failing_addresses(),
                block_index: self.block_index.load(Ordering::Acquire).to_string(),
                current_address_index: self.block_offset().to_string(),
            }),
        };
        Status { class: self.strategy.class(), details }
    }

    pub(crate) fn free_address(&self, address: IpAddr) {
        locked(&self.failing).remove(&address);
    }

    pub(crate) fn free_all(&self) {
        locked(&self.failing).clear();
    }

    fn rotating_address(&self) -> Option<IpAddr> {
        let index = self.index.load(Ordering::Acquire);
        if index == 0 || self.next.swap(false, Ordering::AcqRel) {
            return self.select_rotating_address(index);
        }
        self.block.address_at(u128::from(index - 1))
    }

    /// Look for the next non-excluded, non-failing address after `selected`, storing its one-based
    /// index like the reference's `extractLocalAddress`.
    fn select_rotating_address(&self, selected: u64) -> Option<IpAddr> {
        let limit = self.excluded.len() + locked(&self.failing).len() + 1;
        for offset in 0..limit {
            let raw = selected.checked_add(1)?.checked_add(offset as u64)?;
            let index = match self.block.size() {
                Some(size) if size > 0 => u128::from(raw - 1) % size,
                _ => u128::from(raw - 1),
            };
            let address = self.block.address_at(index)?;
            if self.is_available(address) {
                let stored = u64::try_from(index + 1).unwrap_or(u64::MAX);
                self.index.store(stored, Ordering::Release);
                return Some(address);
            }
        }
        None
    }

    fn balancing_address(&self) -> Option<IpAddr> {
        let limit = self.excluded.len() + 1;
        for _ in 0..limit {
            let index = match self.block.size() {
                Some(size) => self.random_u128() % size,
                None => self.random_u128(),
            };
            let address = self.block.address_at(index)?;
            if self.is_available(address) {
                return Some(address);
            }
        }
        None
    }

    fn nano_address(&self) -> Option<IpAddr> {
        let offset = self.nano_offset();
        if !self.block.at_least_block64() {
            return self.block.address_at(offset);
        }
        let high = match self.block.size() {
            Some(size) if size > BLOCK64 => (self.random_u128() % (size / BLOCK64)) * BLOCK64,
            Some(_) => 0,
            None => self.random_u128() & !(BLOCK64 - 1),
        };
        self.block.address_at(high | (offset & (BLOCK64 - 1)))
    }

    fn rotating_nano_address(&self) -> Option<IpAddr> {
        let block = u128::from(self.block_index.load(Ordering::Acquire));
        let index = block.wrapping_mul(BLOCK64).wrapping_add(self.block_offset());
        if let Some(address) = self.block.address_at(index) {
            return Some(address);
        }
        self.block_index.store(0, Ordering::Release);
        self.block.address_at(self.block_offset())
    }

    fn nano_offset(&self) -> u128 {
        self.nano_start.elapsed().as_nanos()
    }

    fn block_offset(&self) -> u128 {
        let started = u128::from(self.block_start.load(Ordering::Acquire));
        self.nano_start.elapsed().as_nanos().saturating_sub(started)
    }

    fn random_u128(&self) -> u128 {
        let mut bytes = [0_u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            return 0;
        }
        u128::from_be_bytes(bytes)
    }

    fn is_available(&self, address: IpAddr) -> bool {
        if self.excluded.contains(&address) {
            return false;
        }
        let mut failing = locked(&self.failing);
        match failing.get(&address).copied() {
            None => true,
            Some(timestamp) if timestamp.saturating_add(FAILING_MS) < now_millis() as i64 => {
                failing.remove(&address);
                true
            }
            Some(_) => false,
        }
    }

    fn failing_addresses(&self) -> Vec<FailingAddress> {
        locked(&self.failing)
            .iter()
            .map(|(address, timestamp)| FailingAddress {
                failing_address: address.to_string(),
                failing_timestamp: *timestamp,
                failing_time: java_date(*timestamp),
            })
            .collect()
    }

    /// The reference marks the address that was used and asks the planner for the next one.
    #[cfg(test)]
    fn mark_failing(&self, address: IpAddr, timestamp: i64) {
        locked(&self.failing).insert(address, timestamp);
        match self.strategy {
            Strategy::RotateOnBan => {
                self.rotate_index.fetch_add(1, Ordering::AcqRel);
                self.next.store(true, Ordering::Release);
            }
            Strategy::RotatingNanoSwitch => {
                self.block_index.fetch_add(1, Ordering::AcqRel);
                let now = self.nano_start.elapsed().as_nanos() as u64;
                self.block_start.store(now, Ordering::Release);
            }
            Strategy::LoadBalance | Strategy::NanoSwitch => {}
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Status {
    #[serde(rename = "class")]
    class: &'static str,
    details: Details,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Details {
    RotatingIp(RotatingIpDetails),
    Nano(NanoIpDetails),
    RotatingNano(RotatingNanoDetails),
    Balancing(BalancingIpDetails),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RotatingIpDetails {
    ip_block: BlockStatus,
    failing_addresses: Vec<FailingAddress>,
    rotate_index: String,
    ip_index: String,
    current_address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NanoIpDetails {
    ip_block: BlockStatus,
    failing_addresses: Vec<FailingAddress>,
    current_address_index: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RotatingNanoDetails {
    ip_block: BlockStatus,
    failing_addresses: Vec<FailingAddress>,
    block_index: String,
    current_address_index: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BalancingIpDetails {
    ip_block: BlockStatus,
    failing_addresses: Vec<FailingAddress>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlockStatus {
    #[serde(rename = "type")]
    kind: &'static str,
    size: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FailingAddress {
    failing_address: String,
    failing_timestamp: i64,
    failing_time: String,
}

/// `java.util.Date::toString`, rendered in UTC (the reference uses the JVM's default zone; Kairo
/// has no timezone database and keeps the same layout without a new dependency).
fn java_date(timestamp: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let seconds = timestamp.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let weekday = WEEKDAYS[days.rem_euclid(7) as usize];
    format!(
        "{weekday} {} {day:>2} {:02}:{:02}:{:02} UTC {year}",
        MONTHS[month as usize - 1],
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// Howard Hinnant's `civil_from_days`, the inverse of a days-since-epoch count.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
#[path = "tests/route_planner.rs"]
mod tests;
