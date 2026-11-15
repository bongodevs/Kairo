//! Native-library header check, run on the staged copy before it is loaded.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Enough leading bytes to tell the three executable formats apart.
const MAGIC_BYTES: u64 = 8;

const MACH_O: [&[u8; 4]; 6] = [
    b"\xfe\xed\xfa\xce",
    b"\xfe\xed\xfa\xcf",
    b"\xce\xfa\xed\xfe",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
    b"\xbe\xba\xfe\xca",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Elf,
    Pe,
    MachO,
}

impl Format {
    fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x7fELF") {
            Some(Self::Elf)
        } else if bytes.starts_with(b"MZ") {
            Some(Self::Pe)
        } else if MACH_O.iter().any(|magic| bytes.starts_with(*magic)) {
            Some(Self::MachO)
        } else {
            None
        }
    }

    const fn os(self) -> &'static str {
        match self {
            Self::Elf => "Linux",
            Self::Pe => "Windows",
            Self::MachO => "macOS",
        }
    }
}

const fn host_format() -> Format {
    if cfg!(target_os = "windows") {
        Format::Pe
    } else if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
        Format::MachO
    } else {
        Format::Elf
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum NativeError {
    #[error("plugin {name:?} is built for {built_for}, but this server runs on {running_on}")]
    WrongPlatform { name: String, built_for: &'static str, running_on: &'static str },
    #[error("plugin {name:?} is not a native library for {running_on}")]
    Unrecognized { name: String, running_on: &'static str },
    #[error("read plugin {name:?} at {path}")]
    Read {
        name: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Reject a file whose header is not a native library for this OS.
pub(super) fn check(path: &Path, name: &str) -> Result<(), NativeError> {
    let running_on = host_format().os();
    let format = read_header(path).map_err(|source| NativeError::Read {
        name: name.to_owned(),
        path: path.to_owned(),
        source,
    })?;
    match format {
        None => Err(NativeError::Unrecognized { name: name.to_owned(), running_on }),
        Some(format) if format != host_format() => Err(NativeError::WrongPlatform {
            name: name.to_owned(),
            built_for: format.os(),
            running_on,
        }),
        Some(_) => Ok(()),
    }
}

fn read_header(path: &Path) -> io::Result<Option<Format>> {
    let file = File::open(path)?;
    let mut magic = Vec::with_capacity(MAGIC_BYTES as usize);
    file.take(MAGIC_BYTES).read_to_end(&mut magic)?;
    Ok(Format::detect(&magic))
}

#[cfg(test)]
#[path = "../tests/plugins_native.rs"]
mod tests;
