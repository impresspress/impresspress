//! The sandbox mode a runtime wasm was compiled as, read from its bytes at
//! bundle time.
//!
//! Every browser runtime declares its mode in a custom section named
//! [`SANDBOX_MODE_SECTION`], whose payload is the mode's spelling and nothing
//! else. `impresspress-browser` writes it (`src/sandbox_mode.rs`, the
//! `link_section` static that `compiled_sandbox_mode()` reads), from
//! `impresspress-core`'s `SandboxMode::COMPILED`. This crate cannot depend on
//! either — it is native tooling, and they are the runtime — so the section's
//! name is spelled twice, and the real-artifact checks hold the two
//! spellings together: `crates/impresspress/tests/wasm_bundling.rs` reads the
//! wasm baked into the CLI with [`compiled_sandbox_mode`], and every real
//! bundle CI builds goes through [`super::run`], which refuses a wasm without
//! the section.
//!
//! The reader is a section walker, not a wasm parser: it checks the module
//! header, then steps over every section by its declared size, and looks
//! inside custom sections only. A module whose sections do not tile its bytes
//! exactly is refused, so a truncated or padded file is never read as a
//! runtime that declares something.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use super::SandboxShell;

/// The custom section a runtime wasm declares its sandbox mode in.
/// `impresspress-browser`'s `SANDBOX_MODE_SECTION` static is emitted under
/// this name (its `link_section` attribute spells it again).
pub const SANDBOX_MODE_SECTION: &str = "impresspress.sandbox_mode";

/// The first eight bytes of every version-1 wasm module: `\0asm`, then the
/// version, little-endian.
const HEADER: [u8; 8] = *b"\0asm\x01\0\0\0";

/// Why a wasm's sandbox mode could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnreadableMode {
    /// The bytes do not start with the version-1 wasm header.
    NotWasm,
    /// The bytes start like a module but their sections do not hold
    /// together; the detail says where.
    Malformed(String),
    /// The module has no [`SANDBOX_MODE_SECTION`] section.
    Undeclared,
    /// The module has this many [`SANDBOX_MODE_SECTION`] sections, not one.
    Repeated(usize),
    /// The one section's payload, which is not a mode's spelling. Two
    /// declarations merged at link time read as their concatenation
    /// (`"workspaceworkspace"`), and land here.
    Unknown(Vec<u8>),
}

impl fmt::Display for UnreadableMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWasm => write!(
                f,
                "is not a wasm module (it does not start with the version-1 `\\0asm` header)"
            ),
            Self::Malformed(detail) => write!(f, "is not a well-formed wasm module: {detail}"),
            Self::Undeclared => write!(
                f,
                "declares no sandbox mode: it has no `{SANDBOX_MODE_SECTION}` custom section"
            ),
            Self::Repeated(count) => write!(
                f,
                "has {count} `{SANDBOX_MODE_SECTION}` custom sections; a runtime declares \
                 exactly one"
            ),
            Self::Unknown(payload) => write!(
                f,
                "declares sandbox mode {:?} in its `{SANDBOX_MODE_SECTION}` section, which is \
                 none of 'none', 'site' and 'workspace'",
                String::from_utf8_lossy(payload)
            ),
        }
    }
}

/// The sandbox mode `wasm` was compiled as: the payload of its one
/// [`SANDBOX_MODE_SECTION`] custom section, which must be exactly one mode's
/// spelling ([`SandboxShell::from_spelling`]).
pub fn compiled_sandbox_mode(wasm: &[u8]) -> Result<SandboxShell, UnreadableMode> {
    let declarations: Vec<&[u8]> = custom_sections(wasm)?
        .into_iter()
        .filter(|section| section.name == SANDBOX_MODE_SECTION.as_bytes())
        .map(|section| section.payload)
        .collect();
    let payload = match declarations[..] {
        [payload] => payload,
        [] => return Err(UnreadableMode::Undeclared),
        _ => return Err(UnreadableMode::Repeated(declarations.len())),
    };
    std::str::from_utf8(payload)
        .ok()
        .and_then(SandboxShell::from_spelling)
        .ok_or_else(|| UnreadableMode::Unknown(payload.to_vec()))
}

/// Why [`super::run`] refused to bundle the runtime it found: the wasm was
/// compiled as another sandbox mode than the bundle is built for, or its mode
/// could not be read. Typed, so a caller that knows where its runtime came
/// from can say how to get the right one (the sealed web flow does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModeRefusal {
    /// The runtime wasm, as found in the directory being bundled.
    pub wasm: PathBuf,
    /// The mode the bundle is built for: `[sandbox] mode`.
    pub configured: SandboxShell,
    /// What the wasm declares instead.
    pub found: RuntimeMode,
}

/// What a refused runtime wasm declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeMode {
    /// A mode, but not the bundle's.
    CompiledAs(SandboxShell),
    /// No mode that can be read.
    Unreadable(UnreadableMode),
}

impl fmt::Display for RuntimeModeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wasm = self.wasm.display();
        let configured = self.configured.as_str();
        match &self.found {
            RuntimeMode::CompiledAs(compiled) => write!(
                f,
                "{wasm} was compiled as sandbox mode '{compiled}', but this bundle is built for \
                 '{configured}' (`[sandbox] mode` in impresspress.toml; no `[sandbox]` table is \
                 'none'), so its service worker would stop before `initialize()`. A runtime's \
                 mode is the `impresspress-core` features its cdylib compiles — neither for \
                 'none', `site-runtime` for 'site', `block-dev` for 'workspace' — so bundle a \
                 runtime built for '{configured}', or change `[sandbox] mode` to match the \
                 runtime.",
                compiled = compiled.as_str(),
            ),
            RuntimeMode::Unreadable(UnreadableMode::Undeclared) => write!(
                f,
                "{wasm} {}, so it cannot be checked against this bundle's sandbox mode \
                 ('{configured}'). Every runtime that links `impresspress-browser` declares its \
                 mode there, and this bundle's service worker imports `compiled_sandbox_mode` \
                 from the runtime's glue, which only such a runtime exports: rebuild the runtime \
                 against this version of `impresspress-browser`.",
                UnreadableMode::Undeclared
            ),
            RuntimeMode::Unreadable(problem) => write!(
                f,
                "{wasm} {problem}, so it cannot be checked against this bundle's sandbox mode \
                 ('{configured}'). The runtime a bundle ships is a wasm-pack output whose module \
                 declares its mode exactly once."
            ),
        }
    }
}

impl std::error::Error for RuntimeModeRefusal {}

/// Refuse `bytes` — the runtime wasm at `wasm` — unless it was compiled as
/// `configured`.
pub fn check(
    wasm: &Path,
    bytes: &[u8],
    configured: SandboxShell,
) -> Result<(), RuntimeModeRefusal> {
    let found = match compiled_sandbox_mode(bytes) {
        Ok(compiled) if compiled == configured => return Ok(()),
        Ok(compiled) => RuntimeMode::CompiledAs(compiled),
        Err(problem) => RuntimeMode::Unreadable(problem),
    };
    Err(RuntimeModeRefusal {
        wasm: wasm.to_path_buf(),
        configured,
        found,
    })
}

/// A custom section of a module: its name's bytes, and the bytes after them.
struct CustomSection<'a> {
    name: &'a [u8],
    payload: &'a [u8],
}

/// Every custom section of `wasm`, in file order.
fn custom_sections(wasm: &[u8]) -> Result<Vec<CustomSection<'_>>, UnreadableMode> {
    let body = wasm
        .strip_prefix(&HEADER[..])
        .ok_or(UnreadableMode::NotWasm)?;
    let mut sections = Vec::new();
    let mut at = 0;
    while at < body.len() {
        // Offsets in messages are the file's, header included.
        let start = HEADER.len() + at;
        let id = body[at];
        at += 1;
        let size = leb128_u32(body, &mut at)
            .map_err(|e| UnreadableMode::Malformed(format!("section at byte {start}: {e}")))?;
        let end = at
            .checked_add(size as usize)
            .filter(|end| *end <= body.len())
            .ok_or_else(|| {
                UnreadableMode::Malformed(format!(
                    "section at byte {start} is {size} bytes long, but only {} remain",
                    body.len() - at
                ))
            })?;
        if id == 0 {
            let section = &body[at..end];
            let mut name_at = 0;
            let name_len = leb128_u32(section, &mut name_at).map_err(|e| {
                UnreadableMode::Malformed(format!("custom section at byte {start}: name {e}"))
            })?;
            let name_end = name_at
                .checked_add(name_len as usize)
                .filter(|name_end| *name_end <= section.len())
                .ok_or_else(|| {
                    UnreadableMode::Malformed(format!(
                        "custom section at byte {start} names {name_len} bytes, but the \
                         section holds only {}",
                        section.len() - name_at
                    ))
                })?;
            sections.push(CustomSection {
                name: &section[name_at..name_end],
                payload: &section[name_end..],
            });
        }
        at = end;
    }
    Ok(sections)
}

/// An unsigned LEB128 `u32` from `bytes` at `*at`, advancing `*at` past it:
/// at most five bytes, and nothing above `u32::MAX`.
fn leb128_u32(bytes: &[u8], at: &mut usize) -> Result<u32, String> {
    let mut value: u64 = 0;
    for shift in (0..5).map(|i| 7 * i) {
        let byte = *bytes.get(*at).ok_or("ends inside its size")?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return u32::try_from(value).map_err(|_| format!("size {value} overflows a u32"));
        }
    }
    Err("size runs past five LEB128 bytes".to_string())
}

/// Well-formed wasm modules, for tests: the one encoder every fixture that
/// [`super::run`] reads is written with, so a fixture is never bytes that only
/// look like a module.
#[cfg(any(test, feature = "test-support"))]
pub mod fixture {
    use super::{SandboxShell, HEADER, SANDBOX_MODE_SECTION};

    /// One custom section: id 0, its size, its name (length-prefixed), then
    /// `payload`.
    pub fn custom_section(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut content = leb128(name.len());
        content.extend_from_slice(name.as_bytes());
        content.extend_from_slice(payload);
        let mut section = vec![0];
        section.extend(leb128(content.len()));
        section.extend(content);
        section
    }

    /// A module: the header, then `sections` as given.
    pub fn module(sections: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = HEADER.to_vec();
        for section in sections {
            bytes.extend_from_slice(section);
        }
        bytes
    }

    /// A module whose only section declares `mode`, as a runtime compiled as
    /// `mode` does.
    pub fn runtime_declaring(mode: SandboxShell) -> Vec<u8> {
        module(&[custom_section(
            SANDBOX_MODE_SECTION,
            mode.as_str().as_bytes(),
        )])
    }

    fn leb128(mut value: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                bytes.push(byte);
                return bytes;
            }
            bytes.push(byte | 0x80);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compiled_sandbox_mode,
        fixture::{custom_section, module, runtime_declaring},
        UnreadableMode, SANDBOX_MODE_SECTION,
    };
    use crate::bundle::SandboxShell;

    const MODES: [SandboxShell; 3] = [
        SandboxShell::None,
        SandboxShell::Site,
        SandboxShell::Workspace,
    ];

    #[test]
    fn each_spelling_round_trips() {
        for mode in MODES {
            assert_eq!(SandboxShell::from_spelling(mode.as_str()), Some(mode));
        }
        for other in ["", "None", "workspaceworkspace", "site ", "absent"] {
            assert_eq!(SandboxShell::from_spelling(other), None, "{other:?}");
        }
    }

    #[test]
    fn a_runtime_declaring_each_mode_reads_as_it() {
        for mode in MODES {
            assert_eq!(compiled_sandbox_mode(&runtime_declaring(mode)), Ok(mode));
        }
    }

    /// Other sections — custom ones with other names, and non-custom ones,
    /// whose bytes are never looked into — around the declaration change
    /// nothing.
    #[test]
    fn other_sections_are_stepped_over() {
        // A type section (id 1) with one `() -> ()` function type.
        let types = vec![1, 4, 1, 0x60, 0, 0];
        let wasm = module(&[
            custom_section("producers", b"rustc"),
            types,
            custom_section(SANDBOX_MODE_SECTION, b"site"),
            // A payload that is itself a declaration's bytes, under another
            // name: only the name decides.
            custom_section("impresspress.sandbox_mode2", b"workspace"),
        ]);
        assert_eq!(compiled_sandbox_mode(&wasm), Ok(SandboxShell::Site));
    }

    /// A size of 128 or more takes a second LEB128 byte, and the payload
    /// after a long name is still read from the right place.
    #[test]
    fn multi_byte_sizes_are_read() {
        let long_name = "x".repeat(200);
        let wasm = module(&[
            custom_section(&long_name, &[0xaa; 300]),
            custom_section(SANDBOX_MODE_SECTION, b"workspace"),
        ]);
        // 502 bytes: a two-byte name length, the name, the payload.
        assert_eq!(wasm[9..11], [0xf6, 0x03], "a two-byte section size");
        assert_eq!(compiled_sandbox_mode(&wasm), Ok(SandboxShell::Workspace));
    }

    #[test]
    fn a_module_without_the_section_is_undeclared() {
        assert_eq!(
            compiled_sandbox_mode(&module(&[])),
            Err(UnreadableMode::Undeclared)
        );
        assert_eq!(
            compiled_sandbox_mode(&module(&[custom_section("name", b"app")])),
            Err(UnreadableMode::Undeclared)
        );
    }

    #[test]
    fn two_sections_are_refused() {
        let wasm = module(&[
            custom_section(SANDBOX_MODE_SECTION, b"none"),
            custom_section(SANDBOX_MODE_SECTION, b"none"),
        ]);
        assert_eq!(
            compiled_sandbox_mode(&wasm),
            Err(UnreadableMode::Repeated(2))
        );
    }

    /// What two declarations linked into one section look like.
    #[test]
    fn a_payload_that_is_not_one_spelling_is_refused() {
        for payload in [&b"workspaceworkspace"[..], b"", b"Site", b"none\0"] {
            let wasm = module(&[custom_section(SANDBOX_MODE_SECTION, payload)]);
            assert_eq!(
                compiled_sandbox_mode(&wasm),
                Err(UnreadableMode::Unknown(payload.to_vec())),
                "{payload:?}"
            );
        }
    }

    #[test]
    fn bytes_that_are_not_a_module_are_refused() {
        for bytes in [
            &[0xde, 0xad, 0xbe, 0xef][..],
            b"",
            b"\0asm",
            // Version 2.
            b"\0asm\x02\0\0\0",
        ] {
            assert_eq!(
                compiled_sandbox_mode(bytes),
                Err(UnreadableMode::NotWasm),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn a_section_that_overruns_the_module_is_refused() {
        let mut wasm = runtime_declaring(SandboxShell::None);
        // The declaration's size byte, one more than it holds.
        wasm[9] += 1;
        let Err(UnreadableMode::Malformed(detail)) = compiled_sandbox_mode(&wasm) else {
            panic!("{:?}", compiled_sandbox_mode(&wasm));
        };
        assert!(
            detail.contains("byte 8") && detail.contains("remain"),
            "{detail}"
        );

        // Raw bytes appended after a module: a section id, then a size that
        // claims more than follows.
        let mut appended = runtime_declaring(SandboxShell::None);
        appended.extend_from_slice(b"\0a later build");
        assert!(matches!(
            compiled_sandbox_mode(&appended),
            Err(UnreadableMode::Malformed(_))
        ));
    }

    #[test]
    fn a_size_that_never_ends_is_refused() {
        // A section id, then six continuation bytes.
        let mut wasm = module(&[]);
        wasm.extend_from_slice(&[0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80]);
        let Err(UnreadableMode::Malformed(detail)) = compiled_sandbox_mode(&wasm) else {
            panic!("{:?}", compiled_sandbox_mode(&wasm));
        };
        assert!(detail.contains("five"), "{detail}");

        // …and one cut off by the end of the file.
        let mut cut = module(&[]);
        cut.extend_from_slice(&[0, 0x80]);
        let Err(UnreadableMode::Malformed(detail)) = compiled_sandbox_mode(&cut) else {
            panic!("{:?}", compiled_sandbox_mode(&cut));
        };
        assert!(detail.contains("ends inside"), "{detail}");
    }

    #[test]
    fn a_name_longer_than_its_section_is_refused() {
        // A custom section of 3 bytes whose name claims 10.
        let mut wasm = module(&[]);
        wasm.extend_from_slice(&[0, 3, 10, b'a', b'b']);
        let Err(UnreadableMode::Malformed(detail)) = compiled_sandbox_mode(&wasm) else {
            panic!("{:?}", compiled_sandbox_mode(&wasm));
        };
        assert!(detail.contains("names 10 bytes"), "{detail}");
    }
}
