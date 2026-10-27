//! System runtimes a tool needs (manifest `spec.runtime_deps`, docs/04 §1; docs/03 §1.5):
//! today only the Microsoft Visual C++ v14 Redistributable (x64).
//!
//! # Flavors and versions
//!
//! Every Visual Studio since 2015 ships the **same binary-compatible v14 runtime**
//! (`vcruntime140.dll`, `msvcp140.dll`), registered in one place:
//! `HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64` (`Installed`, `Major`,
//! `Minor`, `Bld`, `Rbld`). A binary built by a toolset needs a runtime **at least as new
//! as that toolset**, so a flavor maps to the minimum minor version of its first toolset:
//!
//! | flavor | manifest id     | Visual Studio | toolset | minimum runtime |
//! |--------|-----------------|---------------|---------|-----------------|
//! | `vc15` | `vcredist-vc15` | 2017 (15.x)   | v141    | 14.10           |
//! | `vs16` | `vcredist-vs16` | 2019 (16.x)   | v142    | 14.20           |
//! | `vs17` | `vcredist-vs17` | 2022 (17.x)   | v143    | 14.30           |
//! | `vs18` | `vcredist-vs18` | 2026 (18.x)   | v145    | 14.50           |
//!
//! (VS 2022 ended at 14.44; VS 2026 ships the 14.50 toolset and runtime, so a `vs18`
//! build on a 14.4x runtime may fail with `STATUS_ENTRYPOINT_NOT_FOUND`.) The newest
//! redistributable satisfies every flavor, so all of them point at the same permanent
//! download link, [`VCREDIST_X64_URL`].
//!
//! [`RuntimeDeps::vcredist_version`] is **blocking** (registry and file reads): call it
//! from `spawn_blocking`.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{ForgeError, Result};

/// Microsoft's permanent link to the newest x64 Visual C++ v14 Redistributable
/// (`vc_redist.x64.exe`, redirects to the current build). Its content changes with every
/// runtime release, so the installer (`vcredist.install` through `forgebox-helper`,
/// docs/11 §1) pins the downloaded file by its sha256 and requires a valid Microsoft
/// Authenticode signature.
pub const VCREDIST_X64_URL: &str = "https://aka.ms/vc14/vc_redist.x64.exe";

/// Signer (simple display name of the Authenticode certificate) the helper requires on
/// `vc_redist.x64.exe` before running it (`vcredist.install`, docs/11 §1).
pub const VCREDIST_PUBLISHER: &str = "Microsoft Corporation";

/// Arguments of an unattended VC++ Redistributable installation: no UI, never reboots.
/// Exit codes 0, 3010 (reboot required) and 1638 (same or newer already installed) mean
/// the runtime is there.
pub const VCREDIST_INSTALL_ARGS: &[&str] = &["/install", "/quiet", "/norestart"];

/// Build flavor of an MSVC-compiled tool (the `compiler` variant of PHP builds:
/// `vc15`, `vs16`, `vs17`, `vs18`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcRedistFlavor {
    Vc15,
    Vs16,
    Vs17,
    Vs18,
}

impl VcRedistFlavor {
    pub const ALL: [Self; 4] = [Self::Vc15, Self::Vs16, Self::Vs17, Self::Vs18];

    /// Wire name (`vs17`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vc15 => "vc15",
            Self::Vs16 => "vs16",
            Self::Vs17 => "vs17",
            Self::Vs18 => "vs18",
        }
    }

    /// Oldest runtime that runs binaries of this flavor (see the module table).
    pub fn min_version(self) -> VcRuntimeVersion {
        let minor = match self {
            Self::Vc15 => 10,
            Self::Vs16 => 20,
            Self::Vs17 => 30,
            Self::Vs18 => 50,
        };
        VcRuntimeVersion::new(14, minor, 0, 0)
    }

    /// Where the user gets a runtime that satisfies this flavor.
    pub fn download_url(self) -> &'static str {
        VCREDIST_X64_URL
    }
}

impl fmt::Display for VcRedistFlavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for VcRedistFlavor {
    type Err = ForgeError;

    /// `vc15`, `vs16`, `vs17`, `vs18` (lowercase, as in build names).
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| ForgeError::InvalidInput(format!("unknown VC++ flavor {s:?}")))
    }
}

/// Version of an installed VC++ runtime (`14.44.35211.0`). Ordered numerically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VcRuntimeVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub revision: u32,
}

impl VcRuntimeVersion {
    pub const fn new(major: u32, minor: u32, build: u32, revision: u32) -> Self {
        Self {
            major,
            minor,
            build,
            revision,
        }
    }

    fn key(&self) -> (u32, u32, u32, u32) {
        (self.major, self.minor, self.build, self.revision)
    }

    /// Whether this runtime runs binaries of `flavor`: a v14 runtime at least as new as
    /// the flavor's toolset (a future v15 runtime would be a different set of DLLs).
    pub fn satisfies(&self, flavor: VcRedistFlavor) -> bool {
        self.major == 14 && *self >= flavor.min_version()
    }
}

impl PartialOrd for VcRuntimeVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VcRuntimeVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

impl fmt::Display for VcRuntimeVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.revision
        )
    }
}

impl FromStr for VcRuntimeVersion {
    type Err = ForgeError;

    /// `14.44.35211.0`, `v14.44.35211.00` (registry `Version` value) or fewer parts
    /// (`14.30`).
    fn from_str(s: &str) -> Result<Self> {
        let invalid = || ForgeError::InvalidInput(format!("invalid VC++ runtime version {s:?}"));
        let text = s.trim().trim_start_matches(['v', 'V']);
        let mut parts = [0_u32; 4];
        let mut count = 0;
        for (slot, piece) in parts.iter_mut().zip(text.split('.')) {
            *slot = piece.parse().map_err(|_| invalid())?;
            count += 1;
        }
        if count < 2 || text.split('.').count() > 4 {
            return Err(invalid());
        }
        Ok(Self::new(parts[0], parts[1], parts[2], parts[3]))
    }
}

impl Serialize for VcRuntimeVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for VcRuntimeVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A system prerequisite the engine knows (manifest `spec.runtime_deps` ids).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuntimeDep {
    /// `vcredist-<flavor>`: the x64 Visual C++ v14 Redistributable, new enough for
    /// `flavor`.
    VcRedist(VcRedistFlavor),
}

impl RuntimeDep {
    /// Manifest id (`vcredist-vs17`).
    pub fn id(&self) -> String {
        match self {
            Self::VcRedist(flavor) => format!("vcredist-{flavor}"),
        }
    }
}

impl fmt::Display for RuntimeDep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id())
    }
}

impl FromStr for RuntimeDep {
    type Err = ForgeError;

    /// A manifest id: `vcredist-vc15`, `vcredist-vs16`, `vcredist-vs17`, `vcredist-vs18`.
    fn from_str(s: &str) -> Result<Self> {
        match s.strip_prefix("vcredist-") {
            Some(flavor) => flavor
                .parse()
                .map(Self::VcRedist)
                .map_err(|_| ForgeError::InvalidInput(format!("unknown runtime dependency {s:?}"))),
            None => Err(ForgeError::InvalidInput(format!(
                "unknown runtime dependency {s:?}"
            ))),
        }
    }
}

/// Detection of system runtimes.
pub trait RuntimeDeps: Send + Sync + fmt::Debug {
    /// Newest x64 Visual C++ v14 runtime installed on the machine, whatever its version;
    /// `None` when there is none. Blocking.
    fn vcredist_version(&self) -> Result<Option<VcRuntimeVersion>>;

    /// The installed runtime when it is new enough for `flavor`; `None` when it is
    /// missing or too old. Blocking.
    fn vcredist_installed(&self, flavor: VcRedistFlavor) -> Result<Option<VcRuntimeVersion>> {
        Ok(self.vcredist_version()?.filter(|v| v.satisfies(flavor)))
    }
}

/// Runtime detection for the running OS.
#[cfg(windows)]
pub fn runtime_deps() -> Arc<dyn RuntimeDeps> {
    Arc::new(super::windows::WindowsRuntimeDeps)
}

/// Runtime detection for the running OS: other systems have no VC++ runtime to detect
/// (the dependency does not apply there).
#[cfg(not(windows))]
pub fn runtime_deps() -> Arc<dyn RuntimeDeps> {
    Arc::new(unsupported::Unsupported)
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;

    #[derive(Debug)]
    pub struct Unsupported;

    impl RuntimeDeps for Unsupported {
        fn vcredist_version(&self) -> Result<Option<VcRuntimeVersion>> {
            Err(ForgeError::UnsupportedPlatform(format!(
                "the Visual C++ Redistributable does not exist on {}",
                std::env::consts::OS
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavors_map_to_minimum_runtimes() {
        let v = |s: &str| s.parse::<VcRuntimeVersion>().unwrap();
        assert_eq!(VcRedistFlavor::Vc15.min_version(), v("14.10"));
        assert_eq!(VcRedistFlavor::Vs16.min_version(), v("14.20"));
        assert_eq!(VcRedistFlavor::Vs17.min_version(), v("14.30"));
        assert_eq!(VcRedistFlavor::Vs18.min_version(), v("14.50"));

        let vs2022_last = v("14.44.35211.0");
        assert!(vs2022_last.satisfies(VcRedistFlavor::Vc15));
        assert!(vs2022_last.satisfies(VcRedistFlavor::Vs16));
        assert!(vs2022_last.satisfies(VcRedistFlavor::Vs17));
        assert!(!vs2022_last.satisfies(VcRedistFlavor::Vs18));
        let vs2026 = v("14.51.36247.0");
        assert!(VcRedistFlavor::ALL.iter().all(|f| vs2026.satisfies(*f)));
        let vs2019 = v("14.29.30133.0");
        assert!(vs2019.satisfies(VcRedistFlavor::Vs16));
        assert!(!vs2019.satisfies(VcRedistFlavor::Vs17));
        assert!(
            !v("15.0").satisfies(VcRedistFlavor::Vc15),
            "not a v14 runtime"
        );
        assert!(
            !v("14.0.24215.1").satisfies(VcRedistFlavor::Vc15),
            "VS 2015"
        );
    }

    #[test]
    fn versions_parse_print_and_order() {
        let v: VcRuntimeVersion = "v14.44.35211.00".parse().unwrap();
        assert_eq!(v, VcRuntimeVersion::new(14, 44, 35211, 0));
        assert_eq!(v.to_string(), "14.44.35211.0");
        assert!(v < "14.50".parse().unwrap());
        assert!(v > "14.44.35208.0".parse().unwrap());
        for bad in ["", "14", "a.b", "14.x", "1.2.3.4.5"] {
            assert!(bad.parse::<VcRuntimeVersion>().is_err(), "{bad:?}");
        }
        let json = serde_json::to_value(v).unwrap();
        assert_eq!(json, "14.44.35211.0");
        assert_eq!(serde_json::from_value::<VcRuntimeVersion>(json).unwrap(), v);
    }

    #[test]
    fn runtime_dep_ids_round_trip() {
        for flavor in VcRedistFlavor::ALL {
            let dep = RuntimeDep::VcRedist(flavor);
            assert_eq!(dep.id().parse::<RuntimeDep>().unwrap(), dep);
            assert_eq!(flavor.as_str().parse::<VcRedistFlavor>().unwrap(), flavor);
            assert_eq!(flavor.download_url(), VCREDIST_X64_URL);
        }
        assert_eq!(
            "vcredist-vs17".parse::<RuntimeDep>().unwrap(),
            RuntimeDep::VcRedist(VcRedistFlavor::Vs17)
        );
        for bad in ["vcredist-vs14", "vcredist", "dotnet-8", "VCREDIST-VS17"] {
            assert_eq!(
                bad.parse::<RuntimeDep>().unwrap_err().code(),
                "invalid_input",
                "{bad}"
            );
        }
        assert_eq!(serde_json::to_value(VcRedistFlavor::Vs18).unwrap(), "vs18");
    }

    #[derive(Debug)]
    struct Fixed(Option<VcRuntimeVersion>);

    impl RuntimeDeps for Fixed {
        fn vcredist_version(&self) -> Result<Option<VcRuntimeVersion>> {
            Ok(self.0)
        }
    }

    #[test]
    fn installed_filters_by_flavor() {
        let old = Fixed(Some(VcRuntimeVersion::new(14, 29, 30133, 0)));
        assert!(
            old.vcredist_installed(VcRedistFlavor::Vs16)
                .unwrap()
                .is_some()
        );
        assert_eq!(old.vcredist_installed(VcRedistFlavor::Vs17).unwrap(), None);
        assert_eq!(
            Fixed(None)
                .vcredist_installed(VcRedistFlavor::Vc15)
                .unwrap(),
            None
        );
    }

    /// Reads the real machine. Every supported Windows machine with a PHP/Apache build
    /// usually has a v14 runtime; when it does not, the test only logs.
    #[cfg(windows)]
    #[test]
    fn detects_the_runtime_of_this_machine() {
        let deps = runtime_deps();
        match deps.vcredist_version().unwrap() {
            Some(version) => {
                eprintln!("VC++ v14 x64 runtime on this machine: {version}");
                assert_eq!(version.major, 14, "{version}");
                assert!(version.minor >= 10, "{version}");
                for flavor in VcRedistFlavor::ALL {
                    let installed = deps.vcredist_installed(flavor).unwrap();
                    assert_eq!(installed.is_some(), version.satisfies(flavor), "{flavor}");
                }
            }
            None => eprintln!("no VC++ v14 x64 runtime installed on this machine"),
        }
    }
}
