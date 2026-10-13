//! Which development-sandbox half this build compiles — decided by this
//! crate's cargo features, and by nothing else.
//!
//! The features are `site-runtime` and `block-dev` (which implies it), and
//! they are read in exactly one place, [`SandboxMode::COMPILED`]. They live
//! here because this is the crate whose code they add or remove; every
//! browser runtime reaches them through cargo's feature unification, however
//! it links this crate:
//!
//! * `impresspress-web`'s `browser-site-runtime` and `browser-devtools`
//!   forward to `site-runtime` and `block-dev` (its own const assertions
//!   check that the two agree in each of its three builds);
//! * an application's own cdylib that never enables either feature
//!   (`examples/minimal-browser` is the in-repo one) compiles
//!   [`SandboxMode::Absent`].
//!
//! `impresspress-browser`, the crate every browser cdylib links, is what
//! declares the mode to the outside: it exports `compiled_sandbox_mode()`
//! to the bundle's `sw.js` and writes the same spelling into the wasm's
//! `impresspress.sandbox_mode` custom section.

/// What the development sandbox contributes to this runtime.
///
/// **Three states, not two, and the middle one is the whole point.** The
/// sandbox is two separable things that used to be one boolean:
///
/// * a **runtime** — seed-on-boot, the generation ledger, journal convergence,
///   the dynamic-block rebuild, and the fact that `/` serves a site rather
///   than bouncing to the login page. Every bundle that has this code
///   compiled in needs it, because it is what makes an ImpressPress folder
///   *serve the site it ships*;
/// * a **workspace** — `/b/dev`, its route, the in-browser compiler, the
///   widened CSP and the cross-origin isolation that compiler needs.
///
/// Each is a cargo feature of this crate (`site-runtime`, and `block-dev`,
/// which implies it), so the three configurations are the three modes, and
/// [`Self::COMPILED`] is the one this binary is. There is no request: a
/// bundle is built for one mode (`[sandbox] mode`), and the runtime declares
/// the one it was compiled as. A mismatch is a bundle assembled from the
/// wrong runtime — a workspace's `sw.js` over a site build, or the reverse.
/// `impresspress build` refuses to assemble one: it reads the wasm's
/// `impresspress.sandbox_mode` section (written by `impresspress-browser`)
/// before it writes the bundle. A bundle put together some other way is
/// caught by its `sw.js`, which states the mode it was built for
/// (`const SANDBOX`) and compares it with `compiled_sandbox_mode()`
/// (exported by `impresspress-browser`) before it calls `initialize()` at
/// all; the worker stops there, with stage `build`, which no recovery erases
/// data for: nothing has opened the database yet, and nothing about the
/// stored data is in question.
///
/// It used to be a request. A devtools build booted with any mode other than
/// `'workspace'` ran as an exported site, the workspace compiled in and
/// unrouted, and that was how the sandbox's export shipped — the sandbox's
/// own runtime, with one line of its `sw.js` rewritten. The export now ships
/// the deployment's export shell, a site build (spec amendment 23), and a
/// devtools build is always a workspace: it can never boot as a site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    /// Neither feature: no sandbox code in the binary at all — the default
    /// `pkg/` every non-sandbox bundle ships, and every embedding cdylib
    /// that does not opt in. The runtime this produces is indistinguishable
    /// from one that never heard of a sandbox — no seeded variables, no
    /// widened CSP, no seed import. "Feature off = nothing" is the security
    /// model (design §13), not an optimisation: a relaxed
    /// `worker-src`/`frame-src` on a build with no sandbox to use them is
    /// pure attack surface.
    Absent,
    /// `site-runtime` alone (`impresspress-web`'s `browser-site-runtime`) —
    /// **an exported site**, the export shell a development sandbox ships.
    /// The runtime half is fully live (it imports its `seed/`, converges its
    /// journal, rebuilds with its dynamic blocks and serves `/`), and none of
    /// the workspace is in the binary: no `/b/dev`, no probe, no export, no
    /// compiler adapter, no widened CSP, and no cross-origin isolation —
    /// which an exported site positively wants back, since isolation is what
    /// stops a page it serves from embedding a third-party iframe (design
    /// §20, amendment 14).
    Site,
    /// `block-dev` (`impresspress-web`'s `browser-devtools`) — the
    /// development sandbox itself. Everything [`Self::Site`] has, plus
    /// `/b/dev` and the tooling around it.
    Workspace,
}

impl SandboxMode {
    /// This build's mode, read from the features once, here.
    ///
    /// Every consumer reaches the features through this constant rather than
    /// restating a `cfg!` of its own, so the rule is one expression; the
    /// const assertions below check it in whichever configuration is being
    /// compiled.
    pub const COMPILED: Self = if cfg!(feature = "block-dev") {
        Self::Workspace
    } else if cfg!(feature = "site-runtime") {
        Self::Site
    } else {
        Self::Absent
    };

    /// Whether the sandbox's **runtime** half is compiled in: the ledger, the
    /// seed import, journal convergence, the dynamic-block rebuild, and the
    /// `HAS_LANDING_PAGE` fact. True for [`Self::Site`] and
    /// [`Self::Workspace`].
    pub const fn runtime_present(self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// Whether the **workspace** half is compiled in: the `/b/dev` route, the
    /// widened CSP, the cross-origin isolation and the framing relaxation.
    /// True only for [`Self::Workspace`].
    pub const fn workspace(self) -> bool {
        matches!(self, Self::Workspace)
    }

    /// The mode's one spelling, the same everywhere it is written: `[sandbox]
    /// mode` in `impresspress.toml`, `impresspress-bundle`'s `SandboxShell`,
    /// `sw.js`'s `const SANDBOX` (`'none'` for a bundle with no `[sandbox]`
    /// table), which the worker compares this with, and the payload of the
    /// wasm's `impresspress.sandbox_mode` section.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "none",
            Self::Site => "site",
            Self::Workspace => "workspace",
        }
    }
}

// The rule, checked by rustc in the configuration actually being compiled.
// One block per configuration; exactly one of them compiles in any build.
// This is the stronger check than a parameterised unit test: it exercises the
// real `cfg!(feature = …)` wiring in `SandboxMode::COMPILED`, in every build
// CI compiles (`Clippy (block-dev)`, `Clippy (site-runtime)`, the default
// build, and every browser runtime).

#[cfg(not(feature = "site-runtime"))]
const _: () = assert!(matches!(SandboxMode::COMPILED, SandboxMode::Absent));

#[cfg(all(feature = "site-runtime", not(feature = "block-dev")))]
const _: () = assert!(matches!(SandboxMode::COMPILED, SandboxMode::Site));

#[cfg(feature = "block-dev")]
const _: () = assert!(matches!(SandboxMode::COMPILED, SandboxMode::Workspace));

// And the predicates agree, in every build.
const _: () = {
    assert!(!SandboxMode::Absent.runtime_present());
    assert!(!SandboxMode::Absent.workspace());
    assert!(SandboxMode::Site.runtime_present());
    assert!(!SandboxMode::Site.workspace());
    assert!(SandboxMode::Workspace.runtime_present());
    assert!(SandboxMode::Workspace.workspace());
};

#[cfg(test)]
mod tests {
    use impresspress_bundle::bundle::SandboxShell;

    use super::SandboxMode;

    /// The runtime writes [`SandboxMode::as_str`] into its
    /// `impresspress.sandbox_mode` section, and `impresspress-bundle` reads
    /// the section back through `SandboxShell::from_spelling` — two crates
    /// that share no type, each spelling the three modes. The bundler refuses
    /// any payload that is not one of its own spellings, so a drift here
    /// would refuse every runtime of that mode at bundle time.
    #[test]
    fn the_runtime_spells_each_mode_as_the_bundler_reads_it() {
        for (mode, shell) in [
            (SandboxMode::Absent, SandboxShell::None),
            (SandboxMode::Site, SandboxShell::Site),
            (SandboxMode::Workspace, SandboxShell::Workspace),
        ] {
            assert_eq!(mode.as_str(), shell.as_str());
            assert_eq!(SandboxShell::from_spelling(mode.as_str()), Some(shell));
        }
    }
}
