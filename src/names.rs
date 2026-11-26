//! Portable component validation, canonical caseless matching, and problem identity.
//!
//! These are lexical checks only. Callers enumerate entries under the workspace
//! lock and still use exclusive creation: a filesystem can have additional aliases.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::str::FromStr;

use icu_casemap::CaseMapper;
use icu_normalizer::DecomposingNormalizer;

/// Unicode version used by [`collision_key`].
///
/// Both `icu_normalizer` and `icu_casemap` are pinned to ICU4X 2.0.0
/// (MSRV 1.82). Their compiled-data dependencies are constrained to the 2.0
/// release series, which uses Unicode 16.0 for both operations. See the
/// [ICU4X 2.0 release](https://github.com/unicode-org/icu4x/releases/tag/icu%402.0.0).
/// Upgrade these dependencies together and review this constant when doing so.
pub const UNICODE_VERSION: &str = "16.0.0";

/// The lexical rule violated by a name. Lengths are UTF-8 byte lengths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong {
        actual_bytes: usize,
        max_bytes: usize,
    },
    BadCharacter(char),
    FirstCharacter(char),
    LastCharacter(char),
    DoubleDot,
    ReservedDeviceName,
    ReservedPrefix,
    DotComponent,
    ControlCharacter(char),
    TrailingDotOrSpace,
    NotUnicode,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("name must not be empty"),
            Self::TooLong {
                actual_bytes,
                max_bytes,
            } => {
                write!(
                    f,
                    "name is {actual_bytes} UTF-8 bytes; maximum is {max_bytes}"
                )
            }
            Self::BadCharacter(c) => write!(f, "name contains a forbidden character: {c:?}"),
            Self::FirstCharacter(c) => {
                write!(
                    f,
                    "identifier must start with an ASCII letter or digit, got {c:?}"
                )
            }
            Self::LastCharacter(c) => write!(f, "identifier must not end with {c:?}"),
            Self::DoubleDot => f.write_str("identifier must not contain '..'"),
            Self::ReservedDeviceName => f.write_str("name is a reserved device name"),
            Self::ReservedPrefix => f.write_str("name starts with the reserved '.cpf-' prefix"),
            Self::DotComponent => f.write_str("component must not be '.' or '..'"),
            Self::ControlCharacter(c) => write!(f, "component contains a control character: {c:?}"),
            Self::TrailingDotOrSpace => f.write_str("component must not end in '.' or U+0020"),
            Self::NotUnicode => f.write_str("name is not valid Unicode"),
        }
    }
}

impl std::error::Error for NameError {}

/// A 1–64 byte ASCII identifier, retaining the user's exact spelling.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identifier(String);

impl Identifier {
    pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
        let name = name.into();
        check_length(&name, 64)?;
        let first = name.chars().next().expect("length checked");
        if !first.is_ascii_alphanumeric() {
            return Err(NameError::FirstCharacter(first));
        }
        if let Some(c) = name
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && !"._-".contains(*c))
        {
            return Err(NameError::BadCharacter(c));
        }
        if name.ends_with('.') {
            return Err(NameError::LastCharacter('.'));
        }
        if name.contains("..") {
            return Err(NameError::DoubleDot);
        }
        check_reserved(&name)?;
        Ok(Self(name))
    }

    /// Validate a native name without substituting replacement characters.
    pub fn from_os_str(name: &OsStr) -> Result<Self, NameError> {
        Self::new(name.to_str().ok_or(NameError::NotUnicode)?)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for Identifier {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for Identifier {
    type Err = NameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

/// A single portable Unicode component, at most 255 bytes as spelled in UTF-8.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortableComponent(String);

impl PortableComponent {
    pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
        let name = name.into();
        check_length(&name, 255)?;
        if name == "." || name == ".." {
            return Err(NameError::DotComponent);
        }
        for c in name.chars() {
            if "/\\:<>\"|?*".contains(c) {
                return Err(NameError::BadCharacter(c));
            }
            if c <= '\u{1f}' || c == '\u{7f}' {
                return Err(NameError::ControlCharacter(c));
            }
        }
        if name.ends_with(['.', ' ']) {
            return Err(NameError::TrailingDotOrSpace);
        }
        check_reserved(&name)?;
        Ok(Self(name))
    }

    /// Validate a native name without substituting replacement characters.
    pub fn from_os_str(name: &OsStr) -> Result<Self, NameError> {
        Self::new(name.to_str().ok_or(NameError::NotUnicode)?)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PortableComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for PortableComponent {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for PortableComponent {
    type Err = NameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

fn check_length(name: &str, max_bytes: usize) -> Result<(), NameError> {
    if name.is_empty() {
        Err(NameError::Empty)
    } else if name.len() > max_bytes {
        Err(NameError::TooLong {
            actual_bytes: name.len(),
            max_bytes,
        })
    } else {
        Ok(())
    }
}

fn check_reserved(name: &str) -> Result<(), NameError> {
    if name
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".cpf-"))
    {
        Err(NameError::ReservedPrefix)
    } else if is_reserved_device_name(name) {
        Err(NameError::ReservedDeviceName)
    } else {
        Ok(())
    }
}

/// Windows device spellings, applied on every platform.
///
/// Legacy devices match the part before the first dot, stripping only trailing
/// U+0020 there. Console names match the whole component, without trimming.
pub fn is_reserved_device_name(name: &str) -> bool {
    if name.eq_ignore_ascii_case("CONIN$") || name.eq_ignore_ascii_case("CONOUT$") {
        return true;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return true;
    }
    let Some(prefix) = stem.get(..3) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case("COM") && !prefix.eq_ignore_ascii_case("LPT") {
        return false;
    }
    matches!(
        &stem[3..],
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

/// Canonical caseless key: NFD(default full CaseFold(NFD(name))).
///
/// This deliberately does not perform compatibility normalization. A key can be
/// longer than the input; component limits apply to the original spelling only.
pub fn collision_key(name: &str) -> String {
    let nfd = DecomposingNormalizer::new_nfd();
    let decomposed = nfd.normalize(name);
    let folded = CaseMapper::new().fold_string(&decomposed);
    nfd.normalize(&folded).into_owned()
}

/// Compare a validated new name with an existing native directory entry.
///
/// Existing entries need not pass portable validation. Unicode entries compare
/// by key; invalid Unicode compares only by exact native spelling. No lossy
/// conversion is used, even for entries that cannot collide with a valid name.
pub fn collides_with_native(name: &str, entry: &OsStr) -> bool {
    matches_native_key(name, &collision_key(name), entry)
}

fn matches_native_key(name: &str, key: &str, entry: &OsStr) -> bool {
    match entry.to_str() {
        Some(entry) => key == collision_key(entry),
        None => native_spelling_eq(OsStr::new(name), entry),
    }
}

fn native_spelling_eq(left: &OsStr, right: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        left.as_bytes() == right.as_bytes()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        left.encode_wide().eq(right.encode_wide())
    }
    #[cfg(not(any(unix, windows)))]
    {
        left == right
    }
}

/// A site name: 1–32 lowercase ASCII letters, digits or hyphens, starting
/// with a letter. Grammar validity does not imply that an adapter exists.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Site(String);

impl Site {
    pub fn new(site: &str) -> Result<Self, IdentityError> {
        if site.is_empty()
            || site.len() > 32
            || !site.as_bytes()[0].is_ascii_lowercase()
            || !site
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(IdentityError::InvalidSite(site.to_owned()));
        }
        Ok(Self(site.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Site {
    type Err = IdentityError;

    fn from_str(site: &str) -> Result<Self, Self::Err> {
        Self::new(site)
    }
}

/// A remote problem identity. Its directory is derived without sanitising.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RemoteIdentity {
    site: Site,
    native: String,
    directory: Identifier,
}

impl RemoteIdentity {
    pub fn parse(identity: &str) -> Result<Self, IdentityError> {
        let (site, native) = identity
            .split_once(':')
            .ok_or(IdentityError::MissingSeparator)?;
        let site = Site::new(site)?;
        let (grammar, valid) = match site.as_str() {
            "atcoder" => (
                "[a-z0-9][a-z0-9_-]{0,55}",
                (1..=56).contains(&native.len())
                    && native
                        .as_bytes()
                        .first()
                        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                    && native.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-')
                    }),
            ),
            "cses" => (
                "[1-9][0-9]{0,6}",
                (1..=7).contains(&native.len())
                    && native
                        .as_bytes()
                        .first()
                        .is_some_and(|b| matches!(b, b'1'..=b'9'))
                    && native.bytes().all(|b| b.is_ascii_digit()),
            ),
            "local" => return Err(IdentityError::LocalNotRemote),
            // No native-id grammar is defined for another site. Do not infer
            // one from the directory grammar or accept an unvalidated identity.
            _ => return Err(IdentityError::UnknownSite(site.to_string())),
        };
        if !valid {
            return Err(IdentityError::InvalidRemoteNative {
                site: site.to_string(),
                grammar,
            });
        }
        let directory =
            Identifier::new(format!("{site}-{native}")).map_err(IdentityError::InvalidNative)?;
        Ok(Self {
            site,
            native: native.to_owned(),
            directory,
        })
    }

    pub fn site(&self) -> &Site {
        &self.site
    }

    pub fn native(&self) -> &str {
        &self.native
    }

    pub fn directory(&self) -> &Identifier {
        &self.directory
    }
}

impl fmt::Display for RemoteIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.site, self.native)
    }
}

impl FromStr for RemoteIdentity {
    type Err = IdentityError;

    fn from_str(identity: &str) -> Result<Self, Self::Err> {
        Self::parse(identity)
    }
}

/// A remote contest identity. Gate 2 supports AtCoder contests only; CSES
/// exposes the problemset and sections, not contests (sites §8.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContestIdentity(RemoteIdentity);

impl ContestIdentity {
    pub fn parse(identity: &str) -> Result<Self, IdentityError> {
        let remote = RemoteIdentity::parse(identity)?;
        if remote.site().as_str() != "atcoder" {
            return Err(IdentityError::UnsupportedContestSite(
                remote.site().to_string(),
            ));
        }
        Ok(Self(remote))
    }

    pub fn directory(&self) -> &Identifier {
        self.0.directory()
    }

    pub fn check_binding(&self, name: &Identifier) -> Result<(), CollisionReason> {
        if self.directory() == name {
            Ok(())
        } else {
            Err(CollisionReason::IdentityMismatch)
        }
    }
}

impl fmt::Display for ContestIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for ContestIdentity {
    type Err = IdentityError;

    fn from_str(identity: &str) -> Result<Self, Self::Err> {
        Self::parse(identity)
    }
}

/// An exact local or remote problem identity, bound to its derived directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProblemIdentity {
    Local(Identifier),
    Remote(RemoteIdentity),
}

impl ProblemIdentity {
    pub fn local(name: Identifier) -> Self {
        Self::Local(name)
    }

    /// Directory name, including the site prefix for remote problems.
    pub fn name(&self) -> &Identifier {
        match self {
            Self::Local(name) => name,
            Self::Remote(remote) => remote.directory(),
        }
    }

    pub fn check_binding(&self, name: &Identifier) -> Result<(), CollisionReason> {
        if self.name() == name {
            Ok(())
        } else {
            Err(CollisionReason::IdentityMismatch)
        }
    }

    pub fn parse(identity: &str) -> Result<Self, IdentityError> {
        let (site, native) = identity
            .split_once(':')
            .ok_or(IdentityError::MissingSeparator)?;
        if site == "local" {
            Identifier::new(native)
                .map(Self::Local)
                .map_err(IdentityError::InvalidNative)
        } else {
            RemoteIdentity::parse(identity).map(Self::Remote)
        }
    }
}

impl FromStr for ProblemIdentity {
    type Err = IdentityError;

    fn from_str(identity: &str) -> Result<Self, Self::Err> {
        Self::parse(identity)
    }
}

impl fmt::Display for ProblemIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(name) => write!(f, "local:{name}"),
            Self::Remote(remote) => write!(f, "{remote}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError {
    MissingSeparator,
    InvalidSite(String),
    UnknownSite(String),
    UnsupportedContestSite(String),
    LocalNotRemote,
    InvalidNative(NameError),
    InvalidRemoteNative { site: String, grammar: &'static str },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => f.write_str("identity must contain ':'"),
            Self::InvalidSite(site) => write!(
                f,
                "invalid identity site {site:?}: expected 1–32 lowercase ASCII letters, digits or hyphens, starting with a letter"
            ),
            Self::UnknownSite(site) => write!(
                f,
                "unknown identity site {site:?}: no native-id grammar is defined"
            ),
            Self::LocalNotRemote => f.write_str("site \"local\" is not a remote site"),
            Self::UnsupportedContestSite(site) => {
                write!(f, "site {site:?} does not support contest identities")
            }
            Self::InvalidNative(error) => write!(f, "invalid identity name: {error}"),
            Self::InvalidRemoteNative { site, grammar } => {
                write!(f, "invalid identity name for {site}: expected {grammar}")
            }
        }
    }
}

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidNative(error) => Some(error),
            _ => None,
        }
    }
}

/// Entry type determined without following links, including Windows reparse points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    Link,
    Other,
}

/// Input classification of a user-editable `problem.toml` under storage §9.
///
/// Classifiers must check schema before fields: `TooNew` carries no decoded
/// identity. `Valid` carries an identity string; directory binding is checked by
/// [`resolve_add`]. Parsing TOML and accessing files are the caller's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProblemMetadata {
    Missing,
    Unreadable,
    Malformed,
    TooNew,
    Valid { identity: String },
}

/// An in-memory description of an entry immediately below `problems/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemEntry {
    pub name: OsString,
    pub kind: EntryKind,
    pub metadata: ProblemMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollisionReason {
    SeveralEntries,
    DifferentSpelling,
    LinkInPath,
    NotADirectory,
    MetadataMissing,
    MetadataUnreadable,
    MetadataMalformed,
    MetadataTooNew,
    IdentityMismatch,
    NativeExists,
}

/// Ordered `cpf add` decision. Collision entries retain their exact native names
/// in listing order; a collision never authorizes reusing an existing entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Create,
    Exists,
    NameCollision {
        reason: CollisionReason,
        entries: Vec<OsString>,
    },
}

/// Resolve a validated problem name against entries enumerated under the lock.
///
/// The first matching rule from storage §2.1 decides. A syntactically invalid
/// identity supplied as `Valid` is treated as malformed metadata (§9).
pub fn resolve_add(name: &Identifier, entries: &[ProblemEntry]) -> Resolution {
    resolve_problem(name, Some(&ProblemIdentity::local(name.clone())), entries)
}

/// Existing-reader lookup accepts any bound identity for a bare directory name.
/// Creation and explicit identity references supply the exact expected identity.
pub fn resolve_problem(
    name: &Identifier,
    expected: Option<&ProblemIdentity>,
    entries: &[ProblemEntry],
) -> Resolution {
    let key = collision_key(name.as_str());
    let matches: Vec<_> = entries
        .iter()
        .filter(|entry| matches_native_key(name.as_str(), &key, &entry.name))
        .collect();
    let entry = match matches.as_slice() {
        [] => return Resolution::Create,
        [entry] => *entry,
        _ => {
            return Resolution::NameCollision {
                reason: CollisionReason::SeveralEntries,
                entries: matches.iter().map(|entry| entry.name.clone()).collect(),
            };
        }
    };
    let reason = if !native_spelling_eq(OsStr::new(name.as_str()), &entry.name) {
        CollisionReason::DifferentSpelling
    } else {
        match entry.kind {
            EntryKind::Link => CollisionReason::LinkInPath,
            EntryKind::Other => CollisionReason::NotADirectory,
            EntryKind::Directory => match &entry.metadata {
                ProblemMetadata::Missing => CollisionReason::MetadataMissing,
                ProblemMetadata::Unreadable => CollisionReason::MetadataUnreadable,
                ProblemMetadata::Malformed => CollisionReason::MetadataMalformed,
                ProblemMetadata::TooNew => CollisionReason::MetadataTooNew,
                ProblemMetadata::Valid { identity } => match ProblemIdentity::parse(identity) {
                    Err(_) => CollisionReason::MetadataMalformed,
                    Ok(identity)
                        if identity.check_binding(name).is_err()
                            || expected.is_some_and(|expected| *expected != identity) =>
                    {
                        CollisionReason::IdentityMismatch
                    }
                    Ok(_) => return Resolution::Exists,
                },
            },
        }
    };
    Resolution::NameCollision {
        reason,
        entries: vec![entry.name.clone()],
    }
}

/// Map exclusive-publication `Exists` with no lexical match to a collision.
///
/// The requested destination is the entry name available when an outside writer
/// or an unmodelled filesystem alias defeats the lexical check. The caller must
/// first reclassify entries with [`resolve_add`], then retain and report the
/// native publication error if no matching entry was found.
pub fn publication_exists(name: &Identifier) -> Resolution {
    Resolution::NameCollision {
        reason: CollisionReason::NativeExists,
        entries: vec![OsString::from(name.as_str())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_validate_each_rule_and_preserve_spelling() {
        for name in [
            "a",
            "Z",
            "0",
            "a_b-c.d",
            "AbC-1_2.3",
            "COM0",
            "LPT0",
            "COM10",
            "COM4x",
        ] {
            let id = Identifier::new(name).unwrap();
            assert_eq!(id.as_str(), name);
            assert_eq!(id.to_string(), name);
            assert_eq!(name.parse::<Identifier>().unwrap(), id);
            assert_eq!(Identifier::from_os_str(OsStr::new(name)).unwrap(), id);
        }
        for (name, error) in [
            ("", NameError::Empty),
            ("_a", NameError::FirstCharacter('_')),
            ("-a", NameError::FirstCharacter('-')),
            (".", NameError::FirstCharacter('.')),
            ("..", NameError::FirstCharacter('.')),
            (".a", NameError::FirstCharacter('.')),
            ("é", NameError::FirstCharacter('é')),
            ("aé", NameError::BadCharacter('é')),
            ("a/b", NameError::BadCharacter('/')),
            ("a\\b", NameError::BadCharacter('\\')),
            ("a:b", NameError::BadCharacter(':')),
            ("a b", NameError::BadCharacter(' ')),
            ("a\0b", NameError::BadCharacter('\0')),
            ("a.", NameError::LastCharacter('.')),
            ("a..b", NameError::DoubleDot),
            ("con", NameError::ReservedDeviceName),
            ("NUL.txt", NameError::ReservedDeviceName),
            ("lPt9", NameError::ReservedDeviceName),
        ] {
            assert_eq!(Identifier::new(name), Err(error.clone()), "{name:?}");
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn portable_components_validate_each_rule_and_preserve_spelling() {
        for name in [
            "a",
            "é",
            "文件",
            "a..b",
            "...a",
            ".hidden",
            "two words.txt",
            " leading",
            "a\u{a0}",
            "a\u{80}",
            "CONIN$.txt",
            "CONOUT$.txt",
        ] {
            let component = PortableComponent::new(name).unwrap();
            assert_eq!(component.as_str(), name);
            assert_eq!(component.to_string(), name);
            assert_eq!(name.parse::<PortableComponent>().unwrap(), component);
            assert_eq!(
                PortableComponent::from_os_str(OsStr::new(name)).unwrap(),
                component
            );
        }
        assert_eq!(PortableComponent::new(""), Err(NameError::Empty));
        for name in [".", ".."] {
            assert_eq!(PortableComponent::new(name), Err(NameError::DotComponent));
        }
        for c in "/\\:<>\"|?*".chars() {
            assert_eq!(
                PortableComponent::new(format!("a{c}b")),
                Err(NameError::BadCharacter(c))
            );
        }
        for c in ('\0'..='\u{1f}').chain(std::iter::once('\u{7f}')) {
            assert_eq!(
                PortableComponent::new(format!("a{c}b")),
                Err(NameError::ControlCharacter(c))
            );
        }
        for name in ["a.", "a ", "a. ", "a ."] {
            assert_eq!(
                PortableComponent::new(name),
                Err(NameError::TrailingDotOrSpace)
            );
        }
    }

    #[test]
    fn lengths_apply_to_original_utf8_spelling() {
        assert!(Identifier::new("a".repeat(64)).is_ok());
        assert_eq!(
            Identifier::new("a".repeat(65)),
            Err(NameError::TooLong {
                actual_bytes: 65,
                max_bytes: 64,
            })
        );
        let limit = format!("{}a", "é".repeat(127));
        assert_eq!(limit.len(), 255);
        assert!(PortableComponent::new(limit).is_ok());
        assert_eq!(
            PortableComponent::new("é".repeat(128)),
            Err(NameError::TooLong {
                actual_bytes: 256,
                max_bytes: 255,
            })
        );
        let expands = "\u{0390}".repeat(127);
        assert_eq!(expands.len(), 254);
        assert!(PortableComponent::new(expands.clone()).is_ok());
        assert!(collision_key(&expands).len() > 255);
        assert!(PortableComponent::new("a".repeat(255)).is_ok());
        assert!(matches!(
            PortableComponent::new("a".repeat(256)),
            Err(NameError::TooLong { .. })
        ));
    }

    #[test]
    fn reserved_devices_use_first_dot_ascii_case_and_only_ascii_space_trimming() {
        let mut reserved: Vec<String> = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        for prefix in ["COM", "LPT"] {
            for suffix in ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"] {
                reserved.push(format!("{prefix}{suffix}"));
            }
        }
        for device in reserved {
            for spelling in [
                device.clone(),
                device.to_ascii_lowercase(),
                mixed_ascii_case(&device),
            ] {
                assert!(is_reserved_device_name(&spelling), "{spelling:?}");
                assert_eq!(
                    PortableComponent::new(&spelling),
                    Err(NameError::ReservedDeviceName)
                );
                if spelling.is_ascii() && !spelling.contains('$') {
                    assert_eq!(
                        Identifier::new(&spelling),
                        Err(NameError::ReservedDeviceName)
                    );
                }
                if !device.contains('$') {
                    for suffix in [".txt", " .txt", "   .txt", ".tar.gz"] {
                        let name = format!("{spelling}{suffix}");
                        assert!(is_reserved_device_name(&name), "{name:?}");
                        assert_eq!(
                            PortableComponent::new(name),
                            Err(NameError::ReservedDeviceName)
                        );
                    }
                    assert!(is_reserved_device_name(&format!("{spelling}  ")));
                }
            }
        }
        for name in [
            "COM0",
            "LPT0",
            "COM10",
            "LPT10",
            "COM4x",
            "COM⁴",
            "LPT⁴",
            "CON\u{a0}.txt",
            "CONIN$.txt",
            "CONOUT$.txt",
            "CONIN$ .txt",
            "x.CON",
            "CONx",
            "COM",
            "LPT",
            " CON.txt",
            "CON\t.txt",
            "éé",
        ] {
            for spelling in [
                name.to_owned(),
                name.to_ascii_lowercase(),
                mixed_ascii_case(name),
            ] {
                assert!(!is_reserved_device_name(&spelling), "{spelling:?}");
            }
            if !name.contains('\t') {
                assert!(PortableComponent::new(name).is_ok(), "{name:?}");
            }
        }
        // Console devices do not use the legacy devices' trimming rule.
        assert!(!is_reserved_device_name("CONIN$ "));
        assert!(!is_reserved_device_name("CONOUT$ "));
    }

    fn mixed_ascii_case(s: &str) -> String {
        s.chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect()
    }

    #[test]
    fn reserved_prefix_is_ascii_case_insensitive() {
        for name in [
            ".cpf-x",
            ".CPF-tmp",
            ".cPf-",
            ".cpf-tmp-0123456789abcdef0123456789abcdef",
        ] {
            assert_eq!(PortableComponent::new(name), Err(NameError::ReservedPrefix));
            assert_eq!(Identifier::new(name), Err(NameError::FirstCharacter('.')));
        }
        assert!(PortableComponent::new(".cpfx").is_ok());
        assert_eq!(
            Identifier::new(".cpfx"),
            Err(NameError::FirstCharacter('.'))
        );
        for name in ["cpf-x", "x.cpf-x", ".cpf", ".cpｆ-x"] {
            assert!(PortableComponent::new(name).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn keys_use_canonical_default_full_folding() {
        for (left, right, key) in [
            ("ABC", "abc", "abc"),
            ("é", "e\u{0301}", "e\u{0301}"),
            ("ß", "ss", "ss"),
            ("ǅ", "ǆ", "ǆ"),
            ("\u{03ca}\u{0301}", "\u{03aa}\u{0301}", "ι\u{0308}\u{0301}"),
            (
                "a\u{0301}\u{0323}",
                "a\u{0323}\u{0301}",
                "a\u{0323}\u{0301}",
            ),
            ("\u{1f80}", "\u{1f00}\u{0345}", "α\u{0313}ι"),
            ("α\u{0345}\u{0301}", "α\u{0301}\u{0345}", "α\u{0301}ι"),
            ("K", "\u{212a}", "k"),
        ] {
            assert_eq!(collision_key(left), key, "{left:?}");
            assert_eq!(collision_key(right), key, "{right:?}");
        }
        assert_eq!(collision_key("a\u{0301}\u{0300}"), "a\u{0301}\u{0300}");
        assert_eq!(collision_key("a\u{0300}\u{0301}"), "a\u{0300}\u{0301}");
        assert_ne!(
            collision_key("a\u{0301}\u{0300}"),
            collision_key("a\u{0300}\u{0301}")
        );
        assert_ne!(collision_key("①"), collision_key("1"));
        // Default full folding is locale-independent, including dotted I.
        assert_eq!(collision_key("İIı"), "i\u{0307}iı");
        let id = Identifier::new("K").unwrap();
        let component = PortableComponent::new("\u{212a}").unwrap();
        assert!(collides_with_native(
            id.as_str(),
            OsStr::new(component.as_str())
        ));
        assert!(!collides_with_native("a", OsStr::new("b")));
    }

    #[test]
    fn key_is_idempotent_for_generated_unicode_strings() {
        let alphabet = [
            'A',
            'K',
            'ß',
            'ǅ',
            'é',
            '\u{0390}',
            '\u{1f80}',
            'α',
            '\u{0345}',
            '\u{0301}',
            '\u{0300}',
            '\u{0323}',
            '\u{212a}',
            '①',
            'İ',
            'ı',
            'Σ',
            'ς',
            '😀',
            '\0',
            '\u{10ffff}',
        ];
        assert_eq!(collision_key(""), "");
        // A fixed seed keeps failures reproducible without a property-test dependency.
        let mut state = 0x5eeda11_u64;
        for i in 0..2048 {
            let mut input = String::new();
            for j in 0..(i % 32 + 1) {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let c = if j % 2 == 0 {
                    alphabet[(state >> 32) as usize % alphabet.len()]
                } else {
                    char::from_u32((state >> 32) as u32 % 0x110000).unwrap_or('ß')
                };
                input.push(c);
            }
            let key = collision_key(&input);
            assert_eq!(collision_key(&key), key, "{input:?}");
        }
    }

    #[test]
    fn identities_parse_format_and_preserve_spelling() {
        for name in ["a", "Foo-2.bar", "A_1"] {
            let text = format!("local:{name}");
            let identity: ProblemIdentity = text.parse().unwrap();
            assert_eq!(identity.name().as_str(), name);
            assert_eq!(identity.to_string(), text);
            assert_eq!(
                identity,
                ProblemIdentity::local(Identifier::new(name).unwrap())
            );
        }
        for (text, error) in [
            ("local", IdentityError::MissingSeparator),
            ("Local:a", IdentityError::InvalidSite("Local".into())),
            (":a", IdentityError::InvalidSite("".into())),
            ("remote:a", IdentityError::UnknownSite("remote".into())),
            ("local:", IdentityError::InvalidNative(NameError::Empty)),
            (
                "local:a:b",
                IdentityError::InvalidNative(NameError::BadCharacter(':')),
            ),
            (
                "local:con",
                IdentityError::InvalidNative(NameError::ReservedDeviceName),
            ),
        ] {
            assert_eq!(ProblemIdentity::parse(text), Err(error.clone()), "{text:?}");
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn remote_identity_contract_tables() {
        let longest = "a".repeat(56);
        for (text, directory) in [
            ("atcoder:abc400_a".to_owned(), "atcoder-abc400_a".to_owned()),
            ("atcoder:a".to_owned(), "atcoder-a".to_owned()),
            (format!("atcoder:{longest}"), format!("atcoder-{longest}")),
            ("cses:1".to_owned(), "cses-1".to_owned()),
            ("cses:1068".to_owned(), "cses-1068".to_owned()),
            ("cses:9999999".to_owned(), "cses-9999999".to_owned()),
            ("atcoder:0_-9".to_owned(), "atcoder-0_-9".to_owned()),
        ] {
            let identity: ProblemIdentity = text.parse().unwrap();
            let remote: RemoteIdentity = text.parse().unwrap();
            assert_eq!(identity.to_string(), text);
            assert_eq!(remote.to_string(), text);
            assert_eq!(remote.native(), text.split_once(':').unwrap().1);
            assert_eq!(identity.name().as_str(), directory);
            assert_eq!(remote.directory(), identity.name());
            assert_eq!(
                identity.check_binding(&Identifier::new(directory).unwrap()),
                Ok(())
            );
        }
        for text in [
            "atcoder:".to_owned(),
            "atcoder:ABC400_A".to_owned(),
            "atcoder:_a".to_owned(),
            "atcoder:-a".to_owned(),
            "atcoder:a.b".to_owned(),
            format!("atcoder:{}", "a".repeat(57)),
            "cses:0".to_owned(),
            "cses:01".to_owned(),
            "cses:12345678".to_owned(),
            "cses:a".to_owned(),
            "Atcoder:abc400_a".to_owned(),
            "1x:a".to_owned(),
            format!("{}:a", "a".repeat(33)),
            "atcoder:a:b".to_owned(),
            "atcoder:a/b".to_owned(),
            "atcoder:a\\b".to_owned(),
            "atcoder:é".to_owned(),
            "cses:1a".to_owned(),
            "cses:1.2".to_owned(),
            "cses:".to_owned(),
        ] {
            assert!(ProblemIdentity::parse(&text).is_err(), "{text}");
            assert!(RemoteIdentity::parse(&text).is_err(), "{text}");
        }
        let contest: ContestIdentity = "atcoder:abc400".parse().unwrap();
        assert_eq!(contest.to_string(), "atcoder:abc400");
        assert_eq!(contest.directory().as_str(), "atcoder-abc400");
        assert_eq!(
            contest.check_binding(&Identifier::new("atcoder-abc400").unwrap()),
            Ok(())
        );
        assert_eq!(
            contest.check_binding(&Identifier::new("atcoder-abc401").unwrap()),
            Err(CollisionReason::IdentityMismatch)
        );
        for text in ["local:c", "cses:1068", "atcoder:ABC400", "atcoder:"] {
            assert!(ContestIdentity::parse(text).is_err(), "{text}");
        }
        assert_eq!(
            RemoteIdentity::parse("local:a"),
            Err(IdentityError::LocalNotRemote)
        );
    }

    #[test]
    fn site_grammar_is_distinct_from_adapter_support() {
        for site in [
            "a".to_owned(),
            "atcoder".to_owned(),
            "cses".to_owned(),
            "local".to_owned(),
            "x-1-".to_owned(),
            "a".repeat(32),
        ] {
            let parsed: Site = site.parse().unwrap();
            assert_eq!(parsed.to_string(), site);
        }
        for site in [
            "".to_owned(),
            "A".to_owned(),
            "1x".to_owned(),
            "-x".to_owned(),
            "a_b".to_owned(),
            "a.b".to_owned(),
            "a:b".to_owned(),
            "é".to_owned(),
            "a".repeat(33),
        ] {
            assert!(Site::new(&site).is_err(), "{site}");
        }
        for site in ["other".to_owned(), "x-1".to_owned(), "a".repeat(32)] {
            assert!(Site::new(&site).is_ok());
            assert_eq!(
                ProblemIdentity::parse(&format!("{site}:a")),
                Err(IdentityError::UnknownSite(site))
            );
        }
    }

    #[test]
    fn remote_binding_and_noninjective_derivation_never_reuse_another_identity() {
        for (remote, directory) in [
            ("atcoder:abc400_a", "atcoder-abc400_a"),
            ("cses:1068", "cses-1068"),
        ] {
            let name = Identifier::new(directory).unwrap();
            let expected = ProblemIdentity::parse(remote).unwrap();
            let local = ProblemIdentity::local(name.clone());
            assert_ne!(expected, local);
            assert_eq!(expected.name(), local.name());
            for actual in [
                remote.to_owned(),
                local.to_string(),
                "atcoder:abc400_b".to_owned(),
                "cses:1068".to_owned(),
            ] {
                let identity = ProblemIdentity::parse(&actual).unwrap();
                let entries = [entry(directory, EntryKind::Directory, valid(&actual))];
                assert_eq!(
                    identity.check_binding(&name).is_ok(),
                    *identity.name() == name
                );
                for requested in [None, Some(&expected), Some(&local)] {
                    let result = resolve_problem(&name, requested, &entries);
                    if *identity.name() == name && requested.is_none_or(|value| *value == identity)
                    {
                        assert_eq!(result, Resolution::Exists);
                    } else {
                        assert_eq!(
                            result,
                            Resolution::NameCollision {
                                reason: CollisionReason::IdentityMismatch,
                                entries: vec![directory.into()]
                            }
                        );
                    }
                }
                // `add` rule 6 compares the identity, not just its directory.
                assert_eq!(
                    resolve_add(&name, &entries),
                    resolve_problem(&name, Some(&local), &entries)
                );
            }
            let entries = [entry(directory, EntryKind::Directory, valid(remote))];
            let differently_spelled = Identifier::new(directory.to_ascii_uppercase()).unwrap();
            assert!(matches!(
                resolve_problem(&differently_spelled, Some(&expected), &entries),
                Resolution::NameCollision {
                    reason: CollisionReason::DifferentSpelling,
                    ..
                }
            ));
            let mut several = entries.to_vec();
            several.push(entry(
                &directory.to_ascii_uppercase(),
                EntryKind::Directory,
                valid(remote),
            ));
            assert!(matches!(
                resolve_problem(&name, Some(&expected), &several),
                Resolution::NameCollision {
                    reason: CollisionReason::SeveralEntries,
                    ..
                }
            ));
        }
    }

    fn entry(name: &str, kind: EntryKind, metadata: ProblemMetadata) -> ProblemEntry {
        ProblemEntry {
            name: name.into(),
            kind,
            metadata,
        }
    }

    fn valid(identity: &str) -> ProblemMetadata {
        ProblemMetadata::Valid {
            identity: identity.into(),
        }
    }

    fn collision(reason: CollisionReason, names: &[&str]) -> Resolution {
        Resolution::NameCollision {
            reason,
            entries: names.iter().map(|name| OsString::from(*name)).collect(),
        }
    }

    #[test]
    fn add_resolution_obeys_order_and_reports_all_matching_entries() {
        let name = Identifier::new("foo").unwrap();
        assert_eq!(resolve_add(&name, &[]), Resolution::Create);
        let unrelated = entry("bar", EntryKind::Link, ProblemMetadata::Unreadable);
        assert_eq!(
            resolve_add(&name, std::slice::from_ref(&unrelated)),
            Resolution::Create
        );
        let exact = entry("foo", EntryKind::Directory, valid("local:foo"));
        assert_eq!(
            resolve_add(&name, &[exact.clone(), unrelated.clone()]),
            Resolution::Exists
        );
        let different = entry("Foo", EntryKind::Link, ProblemMetadata::Missing);
        assert_eq!(
            resolve_add(&name, std::slice::from_ref(&different)),
            collision(CollisionReason::DifferentSpelling, &["Foo"],)
        );
        assert_eq!(
            resolve_add(
                &name,
                &[
                    unrelated,
                    different.clone(),
                    exact.clone(),
                    entry("FOO", EntryKind::Other, ProblemMetadata::TooNew)
                ]
            ),
            collision(CollisionReason::SeveralEntries, &["Foo", "foo", "FOO"],)
        );
        // Several entries wins even when none has the requested exact spelling.
        assert_eq!(
            resolve_add(
                &name,
                &[
                    different,
                    entry("FOO", EntryKind::Other, ProblemMetadata::Malformed)
                ]
            ),
            collision(CollisionReason::SeveralEntries, &["Foo", "FOO"],)
        );
        // Spelling wins over every kind and metadata classification.
        for kind in [EntryKind::Directory, EntryKind::Link, EntryKind::Other] {
            for metadata in [
                ProblemMetadata::Missing,
                ProblemMetadata::Unreadable,
                ProblemMetadata::Malformed,
                ProblemMetadata::TooNew,
                valid("local:foo"),
            ] {
                assert_eq!(
                    resolve_add(&name, &[entry("Foo", kind, metadata)]),
                    collision(CollisionReason::DifferentSpelling, &["Foo"],)
                );
            }
        }
        let kelvin = Identifier::new("K").unwrap();
        assert_eq!(
            resolve_add(
                &kelvin,
                &[entry("K", EntryKind::Directory, ProblemMetadata::Missing)]
            ),
            collision(CollisionReason::DifferentSpelling, &["K"])
        );
    }

    #[test]
    fn add_resolution_checks_kind_then_metadata_then_identity() {
        let name = Identifier::new("foo").unwrap();
        let classifications = [
            ProblemMetadata::Missing,
            ProblemMetadata::Unreadable,
            ProblemMetadata::Malformed,
            ProblemMetadata::TooNew,
            valid("local:bar"),
            valid("local:foo"),
        ];
        for metadata in &classifications {
            for (kind, reason) in [
                (EntryKind::Link, CollisionReason::LinkInPath),
                (EntryKind::Other, CollisionReason::NotADirectory),
            ] {
                assert_eq!(
                    resolve_add(&name, &[entry("foo", kind, metadata.clone())]),
                    collision(reason, &["foo"])
                );
            }
        }
        for (metadata, reason) in [
            (ProblemMetadata::Missing, CollisionReason::MetadataMissing),
            (
                ProblemMetadata::Unreadable,
                CollisionReason::MetadataUnreadable,
            ),
            (
                ProblemMetadata::Malformed,
                CollisionReason::MetadataMalformed,
            ),
            (ProblemMetadata::TooNew, CollisionReason::MetadataTooNew),
            (valid("local:bar"), CollisionReason::IdentityMismatch),
            (valid("local:Foo"), CollisionReason::IdentityMismatch),
            (valid("Local:foo"), CollisionReason::MetadataMalformed),
            (valid("local:"), CollisionReason::MetadataMalformed),
        ] {
            assert_eq!(
                resolve_add(&name, &[entry("foo", EntryKind::Directory, metadata)]),
                collision(reason, &["foo"])
            );
        }
        assert_eq!(
            resolve_add(
                &name,
                &[entry("foo", EntryKind::Directory, valid("local:foo"))]
            ),
            Resolution::Exists
        );
        assert_eq!(
            publication_exists(&name),
            collision(CollisionReason::NativeExists, &["foo"])
        );
    }

    fn assert_invalid_unicode_entry(name: OsString, same: OsString, different: OsString) {
        assert!(name.to_str().is_none());
        assert!(native_spelling_eq(&name, &same));
        assert!(!native_spelling_eq(&name, &different));
        assert_eq!(Identifier::from_os_str(&name), Err(NameError::NotUnicode));
        assert_eq!(
            PortableComponent::from_os_str(&name),
            Err(NameError::NotUnicode)
        );
        // Replacement characters are valid Unicode, and must never alias bad native data.
        let replacement = PortableComponent::new("a\u{fffd}").unwrap();
        assert!(!collides_with_native(replacement.as_str(), &name));
        assert!(collides_with_native(
            replacement.as_str(),
            OsStr::new(replacement.as_str())
        ));
        assert!(!collides_with_native("a", &name));
        assert_eq!(
            resolve_add(
                &Identifier::new("a").unwrap(),
                &[ProblemEntry {
                    name,
                    kind: EntryKind::Link,
                    metadata: ProblemMetadata::Unreadable,
                }]
            ),
            Resolution::Create
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_entries_compare_by_exact_native_bytes() {
        use std::os::unix::ffi::OsStringExt;
        assert_invalid_unicode_entry(
            OsString::from_vec(vec![b'a', 0xff]),
            OsString::from_vec(vec![b'a', 0xff]),
            OsString::from_vec(vec![b'a', 0xfe]),
        );
    }

    #[cfg(windows)]
    #[test]
    fn lone_surrogate_entries_compare_by_exact_native_utf16() {
        use std::os::windows::ffi::OsStringExt;
        for surrogate in [0xd800, 0xdc00] {
            assert_invalid_unicode_entry(
                OsString::from_wide(&[b'a' as u16, surrogate]),
                OsString::from_wide(&[b'a' as u16, surrogate]),
                OsString::from_wide(&[b'a' as u16, surrogate + 1]),
            );
        }
    }
}
