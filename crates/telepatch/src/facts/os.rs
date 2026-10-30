//! The `os` fact: family, distro and version parsed from `/etc/os-release`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Os {
    pub family: Family,
    pub distro: Distro,
    pub version: Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Family {
    Debian,
    RedHat,
    Arch,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    Debian,
    Ubuntu,
    PopOs,
    Fedora,
    Rocky,
    Alma,
    Amazon,
    Arch,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    raw: String,
    major: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsParseError(pub String);

impl std::fmt::Display for OsParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot parse os-release: {}", self.0)
    }
}
impl std::error::Error for OsParseError {}

/// Shell command the transport runs to read the os-release file.
pub const READ_COMMAND: &str = "cat /etc/os-release 2>/dev/null || cat /usr/lib/os-release";

impl Version {
    /// The `VERSION_ID` as the host reports it ("" if absent).
    pub fn as_str(&self) -> &str {
        &self.raw
    }
    /// Leading numeric component of the version; `None` on rolling distros.
    pub fn major(&self) -> Option<u32> {
        self.major
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

fn family_of(id: &str) -> Option<Family> {
    match id {
        "debian" | "ubuntu" => Some(Family::Debian),
        "rhel" | "centos" | "fedora" => Some(Family::RedHat),
        "arch" => Some(Family::Arch),
        _ => None,
    }
}

fn distro_of(id: &str) -> Distro {
    match id {
        "debian" => Distro::Debian,
        "ubuntu" => Distro::Ubuntu,
        "pop" => Distro::PopOs,
        "fedora" => Distro::Fedora,
        "rocky" => Distro::Rocky,
        "almalinux" => Distro::Alma,
        "amzn" => Distro::Amazon,
        "arch" => Distro::Arch,
        other => Distro::Other(other.to_string()),
    }
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    let inner = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
    match inner {
        Some(s) => s.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => v.to_string(),
    }
}

impl Os {
    /// Parse the text of an os-release file. `ID` is required.
    pub fn parse(text: &str) -> Result<Os, OsParseError> {
        let (mut id, mut id_like, mut version_id) = (None, String::new(), String::new());
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = unquote(v);
            match k.trim() {
                "ID" => id = Some(v),
                "ID_LIKE" => id_like = v,
                "VERSION_ID" => version_id = v,
                _ => {}
            }
        }
        let id = id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| OsParseError("no ID".into()))?;
        let family = std::iter::once(id.as_str())
            .chain(id_like.split_whitespace())
            .find_map(family_of)
            .unwrap_or_else(|| Family::Other(id.clone()));
        let major = if family == Family::Arch {
            None
        } else {
            let digits: String = version_id
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse().ok()
        };
        Ok(Os {
            family,
            distro: distro_of(&id),
            version: Version {
                raw: version_id,
                major,
            },
        })
    }
}

/// The os-release fixtures, embedded so downstream playbook repos can render
/// plans for every distro without reading telepatch's `tests/` directory.
pub const FIXTURES: &[(&str, &str)] = &[
    (
        "debian12",
        include_str!("../../tests/fixtures/os-release/debian12"),
    ),
    (
        "debian13",
        include_str!("../../tests/fixtures/os-release/debian13"),
    ),
    (
        "ubuntu2404",
        include_str!("../../tests/fixtures/os-release/ubuntu2404"),
    ),
    (
        "ubuntu2604",
        include_str!("../../tests/fixtures/os-release/ubuntu2604"),
    ),
    (
        "pop-os",
        include_str!("../../tests/fixtures/os-release/pop-os"),
    ),
    (
        "fedora43",
        include_str!("../../tests/fixtures/os-release/fedora43"),
    ),
    (
        "fedora44",
        include_str!("../../tests/fixtures/os-release/fedora44"),
    ),
    (
        "rockylinux9",
        include_str!("../../tests/fixtures/os-release/rockylinux9"),
    ),
    (
        "rockylinux10",
        include_str!("../../tests/fixtures/os-release/rockylinux10"),
    ),
    (
        "almalinux9",
        include_str!("../../tests/fixtures/os-release/almalinux9"),
    ),
    (
        "almalinux10",
        include_str!("../../tests/fixtures/os-release/almalinux10"),
    ),
    (
        "amazonlinux2023",
        include_str!("../../tests/fixtures/os-release/amazonlinux2023"),
    ),
    ("arch", include_str!("../../tests/fixtures/os-release/arch")),
    (
        "omarchy",
        include_str!("../../tests/fixtures/os-release/omarchy"),
    ),
    (
        "omarchy-host",
        include_str!("../../tests/fixtures/os-release/omarchy-host"),
    ),
];

impl Os {
    /// The parsed embedded fixture named `name` (`ubuntu2404`, `arch`, ...).
    pub fn fixture(name: &str) -> Option<Os> {
        FIXTURES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, text)| Os::parse(text).unwrap_or_else(|e| panic!("fixture {n}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Distro as D;
    use Family as F;

    fn fixture(name: &str) -> Os {
        let p = format!(
            "{}/tests/fixtures/os-release/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"));
        Os::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    #[test]
    fn os_release_fixture_table() {
        let other = |s: &str| D::Other(s.to_string());
        #[rustfmt::skip]
        let table: Vec<(&str, F, Distro, &str, Option<u32>)> = vec![
            ("debian12",        F::Debian, D::Debian, "12",         Some(12)),
            ("debian13",        F::Debian, D::Debian, "13",         Some(13)),
            ("ubuntu2404",      F::Debian, D::Ubuntu, "24.04",      Some(24)),
            ("ubuntu2604",      F::Debian, D::Ubuntu, "26.04",      Some(26)),
            ("pop-os",          F::Debian, D::PopOs,  "24.04",      Some(24)),
            ("fedora43",        F::RedHat, D::Fedora, "43",         Some(43)),
            ("fedora44",        F::RedHat, D::Fedora, "44",         Some(44)),
            ("rockylinux9",     F::RedHat, D::Rocky,  "9.8",        Some(9)),
            ("rockylinux10",    F::RedHat, D::Rocky,  "10.2",       Some(10)),
            ("almalinux9",      F::RedHat, D::Alma,   "9.8",        Some(9)),
            ("almalinux10",     F::RedHat, D::Alma,   "10.2",       Some(10)),
            ("amazonlinux2023", F::RedHat, D::Amazon, "2023",       Some(2023)),
            ("arch",            F::Arch,   D::Arch,   "20261025.0.616225", None),
            ("omarchy",         F::Arch,   D::Arch,   "20261025.0.616225", None),
            ("omarchy-host",    F::Arch,   other("omarchy"), "4.0.6", None),
        ];
        for (name, family, distro, version, major) in table {
            let os = fixture(name);
            assert_eq!(os.family, family, "{name} family");
            assert_eq!(os.distro, distro, "{name} distro");
            assert_eq!(os.version.as_str(), version, "{name} version");
            assert_eq!(os.version.major(), major, "{name} major");
        }
    }

    #[test]
    fn embedded_fixtures_match_files() {
        let dir = format!("{}/tests/fixtures/os-release", env!("CARGO_MANIFEST_DIR"));
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        on_disk.sort();
        let mut embedded: Vec<&str> = FIXTURES.iter().map(|(n, _)| *n).collect();
        embedded.sort();
        assert_eq!(on_disk, embedded);
        for (n, text) in FIXTURES {
            assert_eq!(
                *text,
                std::fs::read_to_string(format!("{dir}/{n}")).unwrap()
            );
            assert!(Os::fixture(n).is_some());
        }
        assert!(Os::fixture("nope").is_none());
    }

    #[test]
    fn unknown_distro_is_other() {
        let os = Os::parse("ID=alpine\nVERSION_ID=3.20.1\n").unwrap();
        assert_eq!(os.family, F::Other("alpine".into()));
        assert_eq!(os.distro, D::Other("alpine".into()));
        assert_eq!(os.version.major(), Some(3));
    }

    #[test]
    fn missing_id_is_an_error() {
        assert!(Os::parse("NAME=x\n").is_err());
    }
}
