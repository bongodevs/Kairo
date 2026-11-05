use std::net::IpAddr;
use std::sync::atomic::Ordering;

use player::tools::http_config::RoutePlanner;

use super::ip_block::IpBlock;
use super::{IpRoutePlanner, FAILING_TIME_MILLIS};
use crate::config::{RatelimitConfig, RatelimitStrategy};
use crate::protocol::routeplanner::RoutePlannerStatus;
use crate::utils::locked;
use crate::utils::time::now_millis;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn config(blocks: &[&str], excluded: &[&str], strategy: RatelimitStrategy) -> RatelimitConfig {
    RatelimitConfig {
        ip_blocks: blocks.iter().map(|s| s.to_string()).collect(),
        excluded_ips: excluded.iter().map(|s| s.to_string()).collect(),
        strategy,
        ..Default::default()
    }
}

#[test]
fn ipv4_block_sizes_and_wraps() {
    let block = IpBlock::parse("10.0.0.0/30").unwrap();
    assert_eq!(block.size, 4);
    assert_eq!(block.address_at(0), ip("10.0.0.0"));
    assert_eq!(block.address_at(3), ip("10.0.0.3"));
    assert_eq!(block.address_at(5), ip("10.0.0.1")); // wraps by size
}

#[test]
fn ipv4_block_masks_host_bits() {
    // A host address with a /24 prefix is masked down to the network address.
    let block = IpBlock::parse("192.168.1.55/24").unwrap();
    assert_eq!(block.size, 256);
    assert_eq!(block.address_at(0), ip("192.168.1.0"));
}

#[test]
fn ipv6_block_parses() {
    let block = IpBlock::parse("2001:db8::/126").unwrap();
    assert!(block.is_ipv6);
    assert_eq!(block.size, 4);
    assert_eq!(block.address_at(1), ip("2001:db8::1"));
}

#[test]
fn invalid_cidr_is_rejected() {
    assert!(IpBlock::parse("not-an-ip/24").is_none());
    assert!(IpBlock::parse("10.0.0.0/40").is_none());
    assert!(IpBlock::parse("10.0.0.0/x").is_none());
}

// A bare address is a one-address block rather than a config error.
#[test]
fn bare_address_is_a_single_address_block() {
    let v4 = IpBlock::parse("1.2.3.4").unwrap();
    assert_eq!(v4.size, 1);
    assert_eq!(v4.address_at(0), ip("1.2.3.4"));

    let v6 = IpBlock::parse("2001:db8::1").unwrap();
    assert!(v6.is_ipv6);
    assert_eq!(v6.size, 1);
    assert_eq!(v6.address_at(0), ip("2001:db8::1"));
}

#[test]
fn rotate_on_ban_advances_only_when_banned() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/30"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();

    // Sticky: the same address until it's banned.
    assert_eq!(planner.next_address(), Some(ip("10.0.0.0")));
    assert_eq!(planner.next_address(), Some(ip("10.0.0.0")));

    planner.mark_failing(ip("10.0.0.0"));
    assert_eq!(planner.next_address(), Some(ip("10.0.0.1")));
}

#[test]
fn rotate_on_ban_skips_excluded() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/30"],
        &["10.0.0.0"],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    assert_eq!(planner.next_address(), Some(ip("10.0.0.1")));
}

#[test]
fn load_balance_returns_addresses_in_block() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/30"],
        &[],
        RatelimitStrategy::LoadBalance,
    ))
    .unwrap();
    let block = IpBlock::parse("10.0.0.0/30").unwrap();
    let valid: Vec<IpAddr> = (0..4).map(|i| block.address_at(i)).collect();
    for _ in 0..32 {
        let address = planner.next_address().unwrap();
        assert!(valid.contains(&address), "{address} not in block");
    }
}

#[test]
fn free_address_unmarks_failing() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/30"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    planner.mark_failing(ip("10.0.0.0"));
    assert!(planner.free_address(&ip("10.0.0.0")));
    assert!(!planner.free_address(&ip("10.0.0.0"))); // already cleared
    assert!(locked(&planner.failing).is_empty());
}

// A rate-limited address fails on every request already in flight, and those duplicates must
// not each cost a rotation step.
#[test]
fn repeated_bans_of_one_address_rotate_once() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/24"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    let first = planner.next_address().unwrap();

    planner.mark_failing(first);
    planner.mark_failing(first);
    planner.mark_failing(first);
    assert_eq!(planner.rotate_index.load(Ordering::Acquire), 1);

    let second = planner.next_address().unwrap();
    assert_ne!(second, first);
    planner.mark_failing(second);
    assert_eq!(planner.rotate_index.load(Ordering::Acquire), 2);
}

// `rotateIndex` counts bans while `ipIndex` tracks the address offset, and on a block wider than
// 128 addresses a ban steps 10..19 addresses ahead rather than one.
#[test]
fn rotate_on_ban_reports_ban_count_and_address_offset_separately() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/24"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    let first = planner.next_address().unwrap();
    // A second request re-uses the cached pick instead of rescanning.
    assert_eq!(planner.next_address(), Some(first));
    let picked = planner.ip_index.load(Ordering::Acquire);
    assert!((10..=20).contains(&picked), "first pick at {picked}");

    planner.mark_failing(first);
    planner.next_address().unwrap();
    let ip_index = planner.ip_index.load(Ordering::Acquire);
    assert!(ip_index >= picked + 10, "{picked} -> {ip_index}");

    match planner.status() {
        RoutePlannerStatus::RotatingIp(details) => {
            assert_eq!(details.rotate_index, "1");
            assert_eq!(details.ip_index, ip_index.to_string());
        }
        other => panic!("expected RotatingIp, got {other:?}"),
    }
}

#[test]
fn status_reports_rotating_for_rotate_on_ban() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/24"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    match planner.status() {
        RoutePlannerStatus::RotatingIp(details) => {
            assert_eq!(details.ip_block.block_type, "Inet4Address");
            assert_eq!(details.ip_block.size, "256");
        }
        other => panic!("expected RotatingIp, got {other:?}"),
    }
}

#[test]
fn disabled_when_no_blocks() {
    assert!(
        IpRoutePlanner::from_config(&config(&[], &[], RatelimitStrategy::RotateOnBan)).is_none()
    );
}

#[test]
fn status_class_matches_strategy() {
    let cases: [(&[&str], RatelimitStrategy, &str); 4] = [
        (
            &["10.0.0.0/24"],
            RatelimitStrategy::RotateOnBan,
            "RotatingIpRoutePlanner",
        ),
        (
            &["10.0.0.0/24"],
            RatelimitStrategy::LoadBalance,
            "BalancingIpRoutePlanner",
        ),
        (
            &["2001:db8::/48"],
            RatelimitStrategy::NanoSwitch,
            "NanoIpRoutePlanner",
        ),
        (
            &["2001:db8::/48"],
            RatelimitStrategy::RotatingNanoSwitch,
            "RotatingNanoIpRoutePlanner",
        ),
    ];
    for (blocks, strategy, class) in cases {
        let planner = IpRoutePlanner::from_config(&config(blocks, &[], strategy)).unwrap();
        let status = serde_json::to_value(planner.status()).unwrap();
        assert_eq!(status["class"], class, "{strategy:?}");
        assert!(status["details"]["ipBlock"]["size"].is_string(), "{class}");
    }
}

#[test]
fn nano_switch_needs_an_ipv6_64_or_bigger() {
    for blocks in [&["10.0.0.0/8"], &["2001:db8::/96"]] {
        for strategy in [
            RatelimitStrategy::NanoSwitch,
            RatelimitStrategy::RotatingNanoSwitch,
        ] {
            assert!(
                IpRoutePlanner::from_config(&config(blocks, &[], strategy)).is_none(),
                "{blocks:?} accepted for {strategy:?}"
            );
        }
    }
}

#[test]
fn nano_switch_hands_out_a_fresh_address_per_request() {
    let planner = IpRoutePlanner::from_config(&config(
        &["2001:db8::/64"],
        &[],
        RatelimitStrategy::NanoSwitch,
    ))
    .unwrap();
    // The clock is the index, so no two requests share an address without a ban.
    let first = planner.next_address().unwrap();
    assert_ne!(first, planner.next_address().unwrap());
}

#[test]
fn rotating_nano_moves_to_the_next_block_on_ban() {
    let planner = IpRoutePlanner::from_config(&config(
        &["2001:db8::/48"],
        &[],
        RatelimitStrategy::RotatingNanoSwitch,
    ))
    .unwrap();

    let block_of = |address: IpAddr| match address {
        IpAddr::V6(v6) => v6.segments()[3],
        other => panic!("expected IPv6, got {other}"),
    };
    let first = planner.next_address().unwrap();
    assert_eq!(block_of(first), 0);

    planner.mark_failing(first);
    assert_eq!(block_of(planner.next_address().unwrap()), 1);
    match planner.status() {
        RoutePlannerStatus::RotatingNanoIp(details) => {
            assert_eq!(details.block_index, "1");
            assert_eq!(details.failing_addresses.len(), 1);
        }
        other => panic!("expected RotatingNanoIp, got {other:?}"),
    }
}

#[test]
fn failing_addresses_expire() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/30"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    // Backdate the failure past the cache duration: the address is usable again.
    locked(&planner.failing).insert(ip("10.0.0.0"), now_millis() - FAILING_TIME_MILLIS - 1);
    assert!(!planner.is_failing(&ip("10.0.0.0")));
    assert!(locked(&planner.failing).is_empty());
}

// A node nobody polls for status still has to forget expired bans, and a ban is the only moment
// the map grows, so that is where the pruning happens.
#[test]
fn marking_a_ban_drops_the_expired_ones() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/30"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    locked(&planner.failing).insert(ip("10.0.0.3"), now_millis() - FAILING_TIME_MILLIS - 1);

    planner.mark_failing(ip("10.0.0.0"));

    let failing = locked(&planner.failing);
    assert_eq!(failing.len(), 1);
    assert!(failing.contains_key(&ip("10.0.0.0")));
}

// Nothing looks up an address that has stopped failing, so the status walk is what prunes it.
#[test]
fn listing_failing_addresses_drops_the_expired_ones() {
    let planner = IpRoutePlanner::from_config(&config(
        &["10.0.0.0/30"],
        &[],
        RatelimitStrategy::RotateOnBan,
    ))
    .unwrap();
    {
        let mut failing = locked(&planner.failing);
        failing.insert(ip("10.0.0.0"), now_millis() - FAILING_TIME_MILLIS - 1);
        failing.insert(ip("10.0.0.1"), now_millis());
    }
    let listed = planner.failing_addresses();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].failing_address, "10.0.0.1");
    assert_eq!(locked(&planner.failing).len(), 1);
}
