//! Byte-for-byte verification that two snapshots hold the same state.
//!
//! The question a migration has to answer is "did every byte land". This
//! checks it directly: the metadata fidelity fields have to match, and the
//! SHA-256 of each data section (wasm module, wasm memory, stable memory) has
//! to match. The module hash reported by `canister_status` is checked too, so
//! a restored target is confirmed to be running the same code.

use anyhow::Result;
use candid::Principal;
use ic_agent::Agent;
use sha2::{Digest, Sha256};

use crate::ic::{self, Section, SnapshotMetadata};

pub struct Check {
    pub name: String,
    pub pass: bool,
    pub detail: String,
}

impl Check {
    fn new(name: &str, pass: bool, detail: impl Into<String>) -> Self {
        Check {
            name: name.to_string(),
            pass,
            detail: detail.into(),
        }
    }
}

pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.pass)
    }

    pub fn print(&self) {
        for c in &self.checks {
            let mark = if c.pass { "PASS" } else { "FAIL" };
            println!("  [{mark}] {:<22} {}", c.name, c.detail);
        }
        println!(
            "\n{}",
            if self.passed() {
                "VERIFIED: the target holds the same state as the source."
            } else {
                "MISMATCH: the target does NOT match the source. Do not cut over."
            }
        );
    }
}

fn section_total(meta: &SnapshotMetadata, section: Section) -> u64 {
    match section {
        Section::WasmModule => meta.wasm_module_size,
        Section::WasmMemory => meta.wasm_memory_size,
        Section::StableMemory => meta.stable_memory_size,
    }
}

/// SHA-256 of a whole snapshot section, streamed so a large stable memory is
/// never held in memory at once.
async fn hash_section(
    agent: &Agent,
    canister: Principal,
    snapshot_id: &[u8],
    section: Section,
    total: u64,
) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    ic::read_section(agent, canister, snapshot_id, section, total, |chunk| {
        hasher.update(chunk)
    })
    .await?;
    Ok(hasher.finalize().into())
}

/// Compares a source canister's live module hash against a target's. Run after
/// a load, when the target is meant to carry the same code.
pub async fn module_hash_check(agent: &Agent, source: Principal, target: Principal) -> Result<Check> {
    let (_, src_mod) = ic::status(agent, source).await?;
    let (_, tgt_mod) = ic::status(agent, target).await?;
    Ok(Check::new(
        "module hash (live)",
        src_mod == tgt_mod && src_mod.is_some(),
        match (&src_mod, &tgt_mod) {
            (Some(h), _) => hex::encode(h),
            _ => "source has no module installed".to_string(),
        },
    ))
}

/// Compares the source snapshot against the target snapshot byte-for-byte.
/// This is the transfer check: run it before loading the target.
pub async fn verify_snapshots(
    agent: &Agent,
    source: Principal,
    source_snapshot: &[u8],
    target: Principal,
    target_snapshot: &[u8],
) -> Result<Report> {
    let mut checks = Vec::new();

    let src_meta = ic::snapshot_metadata(agent, source, source_snapshot).await?;
    let tgt_meta = ic::snapshot_metadata(agent, target, target_snapshot).await?;

    // Sizes.
    for (name, a, b) in [
        ("wasm module size", src_meta.wasm_module_size, tgt_meta.wasm_module_size),
        ("wasm memory size", src_meta.wasm_memory_size, tgt_meta.wasm_memory_size),
        ("stable memory size", src_meta.stable_memory_size, tgt_meta.stable_memory_size),
    ] {
        checks.push(Check::new(
            name,
            a == b,
            format!("{a} vs {b} bytes"),
        ));
    }

    // Exported globals, certified data, and the chunk-store hashes.
    checks.push(Check::new(
        "globals",
        src_meta.globals == tgt_meta.globals,
        format!("{} globals", src_meta.globals.len()),
    ));
    checks.push(Check::new(
        "certified data",
        src_meta.certified_data == tgt_meta.certified_data,
        format!("{} bytes", src_meta.certified_data.len()),
    ));
    let src_chunks: Vec<_> = src_meta.wasm_chunk_store.iter().map(|c| &c.hash).collect();
    let tgt_chunks: Vec<_> = tgt_meta.wasm_chunk_store.iter().map(|c| &c.hash).collect();
    checks.push(Check::new(
        "wasm chunk store",
        src_chunks == tgt_chunks,
        format!("{} chunks", src_chunks.len()),
    ));

    // The data sections, hashed. Only hash when the sizes agree. A size
    // mismatch already failed above and the hashes would trivially differ.
    for section in [Section::WasmModule, Section::WasmMemory, Section::StableMemory] {
        let total = section_total(&src_meta, section);
        if total != section_total(&tgt_meta, section) {
            checks.push(Check::new(section.label(), false, "size differs, hash skipped"));
            continue;
        }
        let src_hash = hash_section(agent, source, source_snapshot, section, total).await?;
        let tgt_hash = hash_section(agent, target, target_snapshot, section, total).await?;
        checks.push(Check::new(
            section.label(),
            src_hash == tgt_hash,
            format!("sha256 {}", &hex::encode(src_hash)[..16]),
        ));
    }

    Ok(Report { checks })
}
