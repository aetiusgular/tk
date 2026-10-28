//! Shared types for the toolkit layer. Owned by lane B (wave 1); lane C and
//! lane D read it. The contracts these types follow are in `contracts/`:
//! `policy.md` and `policy.schema.json` for `policy`, `daemon-protocol.md`
//! for `event` and `token`.
//!
//! Session 0 wrote the types, the tool-name normalizer and the reject-list
//! token table, because the contract fixture tests them. The policy matcher
//! (`policy::evaluate`) is a stub that answers `Ask`; lane B fills it against
//! `contracts/fixtures/policy/cases.json`.

pub mod event;
#[rustfmt::skip]
mod fold_tables;
pub mod policy;
pub mod token;
