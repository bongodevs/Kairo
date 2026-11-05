//! IP block representation and CIDR parsing for route planning.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

// Addresses in an IPv6 `/64`, the unit the nano strategies work in. Their clock is a 64-bit
// nanosecond count, so it indexes exactly one of these.
pub(super) const BLOCK64_IPS: u128 = 1u128 << 64;

// A parsed CIDR block.
#[derive(Debug, Clone)]
pub(super) struct IpBlock {
    // Network base address as an integer, IPv4 mapped into the low 32 bits.
    pub(super) base: u128,
    pub(super) is_ipv6: bool,
    // `2^host_bits`, saturated at `u128::MAX`.
    pub(super) size: u128,
}

impl IpBlock {
    // Parse `a.b.c.d/n` or `v6::/n`. A bare address is its own single-address block. `None` for
    // malformed input.
    pub(super) fn parse(cidr: &str) -> Option<Self> {
        let (addr_str, prefix_str) = match cidr.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (cidr, None),
        };
        match addr_str.trim().parse::<IpAddr>().ok()? {
            IpAddr::V4(v4) => {
                let prefix: u32 = match prefix_str {
                    Some(prefix) => prefix.trim().parse().ok()?,
                    None => 32,
                };
                if prefix > 32 {
                    return None;
                }
                let host_bits = 32 - prefix;
                let size = 1u128 << host_bits;
                let mask = size - 1;
                Some(Self {
                    base: (u32::from(v4) as u128) & !mask,
                    is_ipv6: false,
                    size,
                })
            }
            IpAddr::V6(v6) => {
                let prefix: u32 = match prefix_str {
                    Some(prefix) => prefix.trim().parse().ok()?,
                    None => 128,
                };
                if prefix > 128 {
                    return None;
                }
                let host_bits = 128 - prefix;
                let (size, mask) = if host_bits >= 128 {
                    (u128::MAX, u128::MAX)
                } else {
                    let size = 1u128 << host_bits;
                    (size, size - 1)
                };
                Some(Self {
                    base: u128::from(v6) & !mask,
                    is_ipv6: true,
                    size,
                })
            }
        }
    }

    // The address at `index`, wrapping by the block size.
    pub(super) fn address_at(&self, index: u128) -> IpAddr {
        let offset = index % self.size;
        let value = self.base.wrapping_add(offset);
        if self.is_ipv6 {
            IpAddr::V6(Ipv6Addr::from(value))
        } else {
            IpAddr::V4(Ipv4Addr::from((value & 0xFFFF_FFFF) as u32))
        }
    }

    // The address type as the status payload names it.
    pub(super) fn type_name(&self) -> &'static str {
        if self.is_ipv6 {
            "Inet6Address"
        } else {
            "Inet4Address"
        }
    }
}
