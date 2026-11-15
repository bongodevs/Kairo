use std::path::PathBuf;

use super::*;

const ELF: &[u8] = b"\x7fELF\x02\x01\x01\x00";
const PE: &[u8] = b"MZ\x90\x00\x03\x00\x00\x00";
const MACH_O: &[u8] = b"\xcf\xfa\xed\xfe\x07\x00\x00\x01";

#[test]
fn detects_the_three_native_headers() {
    assert_eq!(Format::detect(ELF), Some(Format::Elf));
    assert_eq!(Format::detect(PE), Some(Format::Pe));
    assert_eq!(Format::detect(MACH_O), Some(Format::MachO));
    assert_eq!(Format::detect(b"not a library"), None);
    assert_eq!(Format::detect(b""), None);
}

#[test]
fn a_library_for_this_os_passes() {
    let path = temporary("native");
    std::fs::write(&path, magic(host_format())).expect("write library");
    check(&path, "hello-source").expect("native library");
    std::fs::remove_file(path).expect("cleanup");
}

#[test]
fn wrong_platform_error_names_the_plugin_and_both_systems() {
    let path = temporary("foreign");
    let built_for = foreign_format();
    std::fs::write(&path, magic(built_for)).expect("write library");

    let message = check(&path, "hello-source").expect_err("foreign library").to_string();
    std::fs::remove_file(path).expect("cleanup");

    assert!(message.contains("hello-source"), "{message}");
    assert!(message.contains(built_for.os()), "{message}");
    assert!(message.contains(host_format().os()), "{message}");
}

#[test]
fn an_unknown_header_is_rejected_for_this_os() {
    let path = temporary("unknown");
    std::fs::write(&path, b"plain text").expect("write text file");
    let message = check(&path, "hello-source").expect_err("text file").to_string();
    std::fs::remove_file(path).expect("cleanup");

    assert!(message.contains("hello-source"), "{message}");
    assert!(message.contains(host_format().os()), "{message}");
}

fn foreign_format() -> Format {
    match host_format() {
        Format::Elf => Format::Pe,
        Format::Pe | Format::MachO => Format::Elf,
    }
}

fn magic(format: Format) -> &'static [u8] {
    match format {
        Format::Elf => ELF,
        Format::Pe => PE,
        Format::MachO => MACH_O,
    }
}

fn temporary(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kairo-plugin-{name}-{}.kai", std::process::id()))
}
