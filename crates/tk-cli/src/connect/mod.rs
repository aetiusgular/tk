//! `tk connect` (lane C, T1.2). STUB. Lane C owns this folder.
//! Reads: `contracts/harness-presets.json`, `contracts/docker-args.md`,
//! `contracts/entry.schema.json`, `contracts/policy.md` (the reject list).
//!
//! `gateway/docker_mcp.rs` is the fork's allowlist as copied (NOTICE); it is
//! compiled only with the `fork-docker-mcp` feature until lane C supplies
//! the two helpers it imports from the fork.

#[cfg(feature = "fork-docker-mcp")]
pub mod gateway;

pub fn main(_args: &[String]) -> u8 {
    eprintln!("tk connect: not implemented (session 0 stub; lane C)");
    2
}
