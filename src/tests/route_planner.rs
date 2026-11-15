//! Unit tests for the v4 route planner: config parsing, every strategy's status shape and the
//! failing-address bookkeeping the free endpoints act on.

use std::collections::BTreeSet;
use std::net::IpAddr;

use super::*;

fn config(blocks: &[&str], strategy: &str) -> RateLimitConfig {
    RateLimitConfig {
        ip_blocks: blocks.iter().map(|block| (*block).to_owned()).collect(),
        excluded_ips: Vec::new(),
        strategy: strategy.to_owned(),
        retry_limit: -1,
        search_triggers_fail: true,
    }
}

fn planner(blocks: &[&str], strategy: &str) -> RoutePlanner {
    RoutePlanner::build(Some(&config(blocks, strategy))).expect("valid config").expect("planner")
}

fn status_json(planner: &RoutePlanner) -> serde_json::Value {
    serde_json::to_value(planner.status()).expect("status json")
}

fn keys(value: &serde_json::Value) -> BTreeSet<&str> {
    value.as_object().expect("object").keys().map(String::as_str).collect()
}

#[test]
fn absent_or_empty_blocks_disable_the_planner() {
    assert!(RoutePlanner::build(None).expect("absent block").is_none());
    let empty = config(&[], "RotateOnBan");
    assert!(RoutePlanner::build(Some(&empty)).expect("empty block").is_none());
}

#[test]
fn cidr_sizes_match_the_reference() {
    for (block, kind, size) in [
        ("10.0.0.0/24", "Inet4Address", "256"),
        ("10.0.0.1", "Inet4Address", "1"),
        ("2001:db8::/64", "Inet6Address", "18446744073709551616"),
        ("2001:db8::/0", "Inet6Address", "340282366920938463463374607431768211456"),
    ] {
        let planner = planner(&[block], "RotateOnBan");
        let value = status_json(&planner);
        assert_eq!(value["details"]["ipBlock"]["type"], kind, "{block}");
        assert_eq!(value["details"]["ipBlock"]["size"], size, "{block}");
    }
}

#[test]
fn combined_blocks_are_one_virtual_block() {
    let planner = planner(&["10.0.0.0/31", "10.0.1.0/31"], "RotateOnBan");
    let value = status_json(&planner);
    assert_eq!(value["details"]["ipBlock"]["size"], "4");
    let addresses: Vec<_> = (0..5).map(|index| planner.block.address_at(index)).collect();
    let addresses: Vec<_> =
        addresses.iter().map(|address| address.map(|address| address.to_string())).collect();
    assert_eq!(
        addresses,
        [
            Some(String::from("10.0.0.0")),
            Some(String::from("10.0.0.1")),
            Some(String::from("10.0.1.0")),
            Some(String::from("10.0.1.1")),
            None,
        ]
    );
}

#[test]
fn invalid_configuration_fails_at_startup() {
    for (blocks, strategy) in [
        (&["not-a-block"][..], "RotateOnBan"),
        (&["10.0.0.0/24", "2001:db8::/64"][..], "RotateOnBan"),
        (&["10.0.0.0/24"][..], "RoundRobin"),
        (&["10.0.0.0/24"][..], "NanoSwitch"),
        (&["2001:db8::/80"][..], "NanoSwitch"),
        (&["2001:db8::/80"][..], "RotatingNanoSwitch"),
    ] {
        assert!(
            RoutePlanner::build(Some(&config(blocks, strategy))).is_err(),
            "{strategy} {blocks:?} must be rejected"
        );
    }

    let mut bad_exclusion = config(&["10.0.0.0/24"], "RotateOnBan");
    bad_exclusion.excluded_ips = vec![String::from("not an ip")];
    assert!(RoutePlanner::build(Some(&bad_exclusion)).is_err());
}

#[test]
fn rotating_status_reports_the_raw_index_and_selects_on_first_use() {
    let planner = planner(&["10.0.0.0/24"], "RotateOnBan");
    let value = status_json(&planner);
    assert_eq!(value["class"], "RotatingIpRoutePlanner");
    assert_eq!(
        keys(&value["details"]),
        BTreeSet::from(
            ["currentAddress", "failingAddresses", "ipBlock", "ipIndex", "rotateIndex",]
        )
    );
    assert_eq!(value["details"]["ipIndex"], "0");
    assert_eq!(value["details"]["rotateIndex"], "0");
    assert_eq!(value["details"]["currentAddress"], "null");
    assert_eq!(value["details"]["failingAddresses"], serde_json::json!([]));

    assert_eq!(planner.current_address().map(|a| a.to_string()), Some(String::from("10.0.0.0")));
    assert_eq!(planner.current_address().map(|a| a.to_string()), Some(String::from("10.0.0.0")));
    let value = status_json(&planner);
    assert_eq!(value["details"]["ipIndex"], "1");
    assert_eq!(value["details"]["currentAddress"], "10.0.0.0");
}

#[test]
fn excluded_addresses_are_skipped() {
    let mut config = config(&["10.0.0.0/30"], "RotateOnBan");
    config.excluded_ips = vec![String::from("10.0.0.0"), String::from("10.0.0.1")];
    let planner = RoutePlanner::build(Some(&config)).expect("config").expect("planner");
    assert_eq!(planner.current_address().map(|a| a.to_string()), Some(String::from("10.0.0.2")));
    assert_eq!(status_json(&planner)["details"]["ipIndex"], "3");
}

#[test]
fn failing_addresses_are_reported_and_freed() {
    let planner = planner(&["10.0.0.0/30"], "RotateOnBan");
    let first: IpAddr = planner.current_address().expect("address");
    assert_eq!(first.to_string(), "10.0.0.0");
    planner.mark_failing(first, 1_573_520_707_545);

    let value = status_json(&planner);
    assert_eq!(
        value["details"]["failingAddresses"],
        serde_json::json!([{
            "failingAddress": "10.0.0.0",
            "failingTimestamp": 1_573_520_707_545_i64,
            "failingTime": "Tue Nov 12 01:05:07 UTC 2019",
        }])
    );
    assert_eq!(value["details"]["rotateIndex"], "1");

    // The failed address is skipped by the next request.
    assert_eq!(planner.current_address().map(|a| a.to_string()), Some(String::from("10.0.0.1")));

    planner.free_address(first);
    assert_eq!(status_json(&planner)["details"]["failingAddresses"], serde_json::json!([]));

    planner.mark_failing(first, 1);
    planner.free_all();
    assert_eq!(status_json(&planner)["details"]["failingAddresses"], serde_json::json!([]));
}

#[test]
fn load_balance_has_the_reference_shape() {
    let planner = planner(&["10.0.0.0/24"], "LoadBalance");
    let value = status_json(&planner);
    assert_eq!(value["class"], "BalancingIpRoutePlanner");
    assert_eq!(keys(&value["details"]), BTreeSet::from(["failingAddresses", "ipBlock"]));
    let address = planner.current_address().expect("random address");
    assert!(address.to_string().starts_with("10.0.0."), "{address}");
}

#[test]
fn nano_switch_reports_a_nano_offset() {
    let planner = planner(&["2001:db8::/64"], "NanoSwitch");
    let value = status_json(&planner);
    assert_eq!(value["class"], "NanoIpRoutePlanner");
    assert_eq!(
        keys(&value["details"]),
        BTreeSet::from(["currentAddressIndex", "failingAddresses", "ipBlock"])
    );
    assert!(
        value["details"]["currentAddressIndex"].as_str().expect("index").parse::<u128>().is_ok()
    );
    let address = planner.current_address().expect("nano address");
    assert!(address.to_string().starts_with("2001:db8:"), "{address}");
}

#[test]
fn rotating_nano_switch_reports_the_block_and_offset() {
    let planner = planner(&["2001:db8::/63"], "RotatingNanoSwitch");
    let value = status_json(&planner);
    assert_eq!(value["class"], "RotatingNanoIpRoutePlanner");
    assert_eq!(
        keys(&value["details"]),
        BTreeSet::from(["blockIndex", "currentAddressIndex", "failingAddresses", "ipBlock",])
    );
    assert_eq!(value["details"]["blockIndex"], "0");
    assert!(
        value["details"]["currentAddressIndex"].as_str().expect("index").parse::<u128>().is_ok()
    );

    let address = planner.current_address().expect("nano address");
    planner.mark_failing(address, 1);
    assert_eq!(status_json(&planner)["details"]["blockIndex"], "1");
}
