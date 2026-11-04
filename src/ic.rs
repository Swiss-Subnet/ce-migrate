//! The IC management-canister surface this tool uses, and the direct calls to
//! it.
//!
//! Every call goes to `aaaaa-aa` with the target canister as the effective
//! canister id, so the IC routes it to whichever subnet the canister lives on.
//! The caller only has to be a controller of that canister, which is exactly
//! what we assume and check. No operator proxy is involved: the operator is
//! only needed to *create* the engine canister, which happens before this
//! tool runs.

use anyhow::{anyhow, bail, Context, Result};
use candid::{CandidType, Decode, Encode, Nat, Principal};
use ic_agent::Agent;
use serde::Deserialize;

/// How many bytes of a snapshot section to pull per read. The management
/// canister caps a reply at ~2 MiB, so stay comfortably under it.
pub const CHUNK: u64 = 1024 * 1024;


#[derive(CandidType)]
struct CanisterIdArg {
    canister_id: Principal,
}

#[derive(CandidType, Deserialize)]
struct DefiniteCanisterSettings {
    controllers: Vec<Principal>,
}

#[derive(CandidType, Deserialize)]
struct CanisterStatus {
    settings: DefiniteCanisterSettings,
    module_hash: Option<Vec<u8>>,
}


#[derive(CandidType, Deserialize, Clone)]
pub struct Snapshot {
    pub id: Vec<u8>,
    pub total_size: u64,
    pub taken_at_timestamp: u64,
}

#[derive(CandidType)]
struct TakeSnapshotArg {
    canister_id: Principal,
    replace_snapshot: Option<Vec<u8>>,
}

#[derive(CandidType)]
struct ListSnapshotsArg {
    canister_id: Principal,
}

#[derive(CandidType)]
struct SnapshotMetadataArg {
    canister_id: Principal,
    snapshot_id: Vec<u8>,
}

/// A WebAssembly global, as the metadata reports it. Compared verbatim, so the
/// exact variant and value both have to match.
#[derive(CandidType, Deserialize, PartialEq, Debug, Clone)]
pub enum Global {
    #[serde(rename = "i32")]
    I32(i32),
    #[serde(rename = "i64")]
    I64(i64),
    #[serde(rename = "f32")]
    F32(f32),
    #[serde(rename = "f64")]
    F64(f64),
    #[serde(rename = "v128")]
    V128(Nat),
}

#[derive(CandidType, Deserialize, Clone)]
pub struct ChunkHash {
    pub hash: Vec<u8>,
}

/// The fidelity-bearing parts of a snapshot's metadata. The fields that differ
/// legitimately after an upload (timestamp, canister version, the `source`
/// tag) are not decoded, so a comparison of this struct only ever flags a real
/// difference.
#[derive(CandidType, Deserialize, Clone)]
pub struct SnapshotMetadata {
    pub wasm_module_size: u64,
    pub globals: Vec<Global>,
    pub wasm_memory_size: u64,
    pub stable_memory_size: u64,
    pub wasm_chunk_store: Vec<ChunkHash>,
    pub certified_data: Vec<u8>,
}

#[derive(CandidType, Deserialize)]
enum DataKind {
    #[serde(rename = "wasm_module")]
    WasmModule { offset: u64, size: u64 },
    #[serde(rename = "wasm_memory")]
    WasmMemory { offset: u64, size: u64 },
    #[serde(rename = "stable_memory")]
    StableMemory { offset: u64, size: u64 },
}

#[derive(CandidType)]
struct ReadDataArg {
    canister_id: Principal,
    snapshot_id: Vec<u8>,
    kind: DataKind,
}

#[derive(CandidType, Deserialize)]
struct ReadDataResponse {
    chunk: Vec<u8>,
}

/// Which section of a snapshot to stream.
#[derive(Clone, Copy)]
pub enum Section {
    WasmModule,
    WasmMemory,
    StableMemory,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::WasmModule => "wasm module",
            Section::WasmMemory => "wasm memory",
            Section::StableMemory => "stable memory",
        }
    }

    fn kind(self, offset: u64, size: u64) -> DataKind {
        match self {
            Section::WasmModule => DataKind::WasmModule { offset, size },
            Section::WasmMemory => DataKind::WasmMemory { offset, size },
            Section::StableMemory => DataKind::StableMemory { offset, size },
        }
    }
}

fn mgmt() -> Principal {
    Principal::management_canister()
}

async fn call(agent: &Agent, target: Principal, method: &str, arg: Vec<u8>) -> Result<Vec<u8>> {
    agent
        .update(&mgmt(), method)
        .with_effective_canister_id(target)
        .with_arg(arg)
        .call_and_wait()
        .await
        .with_context(|| format!("management call {method} on {target} failed"))
}

/// The canister's controllers and current module hash.
pub async fn status(agent: &Agent, canister: Principal) -> Result<(Vec<Principal>, Option<Vec<u8>>)> {
    let arg = Encode!(&CanisterIdArg { canister_id: canister })?;
    let bytes = call(agent, canister, "canister_status", arg).await?;
    let status = Decode!(&bytes, CanisterStatus)?;
    Ok((status.settings.controllers, status.module_hash))
}

pub async fn stop(agent: &Agent, canister: Principal) -> Result<()> {
    let arg = Encode!(&CanisterIdArg { canister_id: canister })?;
    call(agent, canister, "stop_canister", arg).await?;
    Ok(())
}

pub async fn start(agent: &Agent, canister: Principal) -> Result<()> {
    let arg = Encode!(&CanisterIdArg { canister_id: canister })?;
    call(agent, canister, "start_canister", arg).await?;
    Ok(())
}

/// Deletes a canister. It has to be stopped first.
pub async fn delete(agent: &Agent, canister: Principal) -> Result<()> {
    let arg = Encode!(&CanisterIdArg { canister_id: canister })?;
    call(agent, canister, "delete_canister", arg).await?;
    Ok(())
}

/// Takes a fresh snapshot and returns it.
pub async fn take_snapshot(agent: &Agent, canister: Principal) -> Result<Snapshot> {
    let arg = Encode!(&TakeSnapshotArg {
        canister_id: canister,
        replace_snapshot: None,
    })?;
    let bytes = call(agent, canister, "take_canister_snapshot", arg).await?;
    Ok(Decode!(&bytes, Snapshot)?)
}

pub async fn list_snapshots(agent: &Agent, canister: Principal) -> Result<Vec<Snapshot>> {
    let arg = Encode!(&ListSnapshotsArg { canister_id: canister })?;
    let bytes = call(agent, canister, "list_canister_snapshots", arg).await?;
    Ok(Decode!(&bytes, Vec<Snapshot>)?)
}

pub async fn snapshot_metadata(
    agent: &Agent,
    canister: Principal,
    snapshot_id: &[u8],
) -> Result<SnapshotMetadata> {
    let arg = Encode!(&SnapshotMetadataArg {
        canister_id: canister,
        snapshot_id: snapshot_id.to_vec(),
    })?;
    let bytes = call(agent, canister, "read_canister_snapshot_metadata", arg).await?;
    Ok(Decode!(&bytes, SnapshotMetadata)?)
}

/// Reads one `size`-byte chunk of a section at `offset`.
pub async fn read_chunk(
    agent: &Agent,
    canister: Principal,
    snapshot_id: &[u8],
    section: Section,
    offset: u64,
    size: u64,
) -> Result<Vec<u8>> {
    let arg = Encode!(&ReadDataArg {
        canister_id: canister,
        snapshot_id: snapshot_id.to_vec(),
        kind: section.kind(offset, size),
    })?;
    let bytes = call(agent, canister, "read_canister_snapshot_data", arg).await?;
    let chunk = Decode!(&bytes, ReadDataResponse)?.chunk;
    if chunk.is_empty() {
        bail!("empty chunk reading {} at offset {offset}", section.label());
    }
    Ok(chunk)
}

/// Streams one section of a snapshot through `sink`, `CHUNK` bytes at a time.
pub async fn read_section(
    agent: &Agent,
    canister: Principal,
    snapshot_id: &[u8],
    section: Section,
    total: u64,
    mut sink: impl FnMut(&[u8]),
) -> Result<()> {
    let mut offset = 0u64;
    while offset < total {
        let size = CHUNK.min(total - offset);
        let chunk = read_chunk(agent, canister, snapshot_id, section, offset, size).await?;
        sink(&chunk);
        offset += chunk.len() as u64;
    }
    Ok(())
}

#[derive(CandidType)]
struct UploadMetadataArg {
    canister_id: Principal,
    replace_snapshot: Option<Vec<u8>>,
    wasm_module_size: u64,
    globals: Vec<Global>,
    wasm_memory_size: u64,
    stable_memory_size: u64,
    certified_data: Vec<u8>,
    global_timer: Option<GlobalTimer>,
    on_low_wasm_memory_hook_status: Option<HookStatus>,
}

#[derive(CandidType, Deserialize, Clone)]
pub enum GlobalTimer {
    #[serde(rename = "inactive")]
    Inactive,
    #[serde(rename = "active")]
    Active(u64),
}

#[derive(CandidType, Deserialize, Clone)]
pub enum HookStatus {
    #[serde(rename = "condition_not_satisfied")]
    ConditionNotSatisfied,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "executed")]
    Executed,
}

#[derive(CandidType, Deserialize)]
struct UploadMetadataResponse {
    snapshot_id: Vec<u8>,
}

#[derive(CandidType, Deserialize)]
enum UploadKind {
    #[serde(rename = "wasm_module")]
    WasmModule { offset: u64 },
    #[serde(rename = "wasm_memory")]
    WasmMemory { offset: u64 },
    #[serde(rename = "stable_memory")]
    StableMemory { offset: u64 },
}

#[derive(CandidType)]
struct UploadDataArg {
    canister_id: Principal,
    snapshot_id: Vec<u8>,
    kind: UploadKind,
    chunk: Vec<u8>,
}

#[derive(CandidType)]
struct LoadSnapshotArg {
    canister_id: Principal,
    snapshot_id: Vec<u8>,
    sender_canister_version: Option<u64>,
}

/// The metadata the target needs to size the snapshot before its bytes arrive.
/// `read`-side metadata has more fields. These are the ones upload takes.
pub struct UploadShape<'a> {
    pub wasm_module_size: u64,
    pub globals: Vec<Global>,
    pub wasm_memory_size: u64,
    pub stable_memory_size: u64,
    pub certified_data: &'a [u8],
}

/// Opens an empty snapshot on the target sized for the incoming state, and
/// returns its id to stream bytes into.
pub async fn upload_metadata(
    agent: &Agent,
    canister: Principal,
    shape: UploadShape<'_>,
) -> Result<Vec<u8>> {
    let arg = Encode!(&UploadMetadataArg {
        canister_id: canister,
        replace_snapshot: None,
        wasm_module_size: shape.wasm_module_size,
        globals: shape.globals,
        wasm_memory_size: shape.wasm_memory_size,
        stable_memory_size: shape.stable_memory_size,
        certified_data: shape.certified_data.to_vec(),
        global_timer: None,
        on_low_wasm_memory_hook_status: None,
    })?;
    let bytes = call(agent, canister, "upload_canister_snapshot_metadata", arg).await?;
    Ok(Decode!(&bytes, UploadMetadataResponse)?.snapshot_id)
}

fn upload_kind(section: Section, offset: u64) -> UploadKind {
    match section {
        Section::WasmModule => UploadKind::WasmModule { offset },
        Section::WasmMemory => UploadKind::WasmMemory { offset },
        Section::StableMemory => UploadKind::StableMemory { offset },
    }
}

/// Writes one chunk of a section into an opened snapshot.
pub async fn upload_data(
    agent: &Agent,
    canister: Principal,
    snapshot_id: &[u8],
    section: Section,
    offset: u64,
    chunk: Vec<u8>,
) -> Result<()> {
    let arg = Encode!(&UploadDataArg {
        canister_id: canister,
        snapshot_id: snapshot_id.to_vec(),
        kind: upload_kind(section, offset),
        chunk,
    })?;
    call(agent, canister, "upload_canister_snapshot_data", arg).await?;
    Ok(())
}

/// Loads a snapshot into the canister, replacing its code and memory.
pub async fn load_snapshot(agent: &Agent, canister: Principal, snapshot_id: &[u8]) -> Result<()> {
    let arg = Encode!(&LoadSnapshotArg {
        canister_id: canister,
        snapshot_id: snapshot_id.to_vec(),
        sender_canister_version: None,
    })?;
    call(agent, canister, "load_canister_snapshot", arg).await?;
    Ok(())
}

/// Finds a canister's single snapshot, erroring if there is not exactly one.
pub async fn sole_snapshot(agent: &Agent, canister: Principal) -> Result<Snapshot> {
    let mut snaps = list_snapshots(agent, canister).await?;
    match snaps.len() {
        1 => Ok(snaps.remove(0)),
        0 => Err(anyhow!("{canister} has no snapshots")),
        n => Err(anyhow!(
            "{canister} has {n} snapshots, name one with --snapshot"
        )),
    }
}
