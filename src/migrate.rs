//! Orchestrates one canister's migration: snapshot the source, stream it into
//! the target, verify the transfer, then load and start the target.
//!
//! What it does NOT do is create the target or preserve the canister id. The
//! target is created beforehand (through the engine operator), and migration
//! across subnets always yields a new id. This tool moves state, and proves it
//! moved intact.

use anyhow::{bail, Result};
use candid::Principal;
use ic_agent::Agent;

use crate::ic::{self, Section, UploadShape};
use crate::verify;

pub struct Options {
    /// Stop the source before snapshotting. On by default. The snapshot itself
    /// is a consistent atomic copy even of a running canister, so this is not
    /// about the copy: it is about divergence. Messages the source processes
    /// after the snapshot create state the migration never carries, so stopping
    /// freezes it at the snapshot.
    pub stop_source: bool,
    /// Load the verified snapshot into the target. Off by default: staging and
    /// verifying leave the target untouched, and loading (which replaces its
    /// state) is a deliberate second step. The target is left STOPPED after a
    /// load, never started automatically.
    pub load: bool,
    /// Start the target after loading. Off by default, so a loaded canister
    /// does not spring to life (running its timers against possibly stale
    /// deposit addresses) before you have cut over. Requires `load`.
    pub start: bool,
    /// Delete the source once the target is loaded and verified. Off by
    /// default, and ignored unless `load` is set. Sweep any tokens the source
    /// holds first. They do not move with a snapshot.
    pub delete_source: bool,
    /// Proceed even if the target already has code or snapshots. Off by
    /// default, so migrating into a canister that is not empty is refused
    /// before anything is touched, since it would overwrite that state.
    pub force: bool,
}

fn controls(controllers: &[Principal], who: Principal) -> bool {
    controllers.contains(&who)
}

/// Fails unless `me` controls `canister`.
///
/// Reading the status already needs control: a non-controller is refused it, so
/// a failure here is translated to the real cause rather than passed through as
/// a raw IC0542 rejection.
async fn ensure_control(agent: &Agent, canister: Principal, me: Principal) -> Result<()> {
    match ic::status(agent, canister).await {
        Ok((controllers, _)) => {
            if controls(&controllers, me) {
                Ok(())
            } else {
                bail!(
                    "{me} can read {canister} but is not in its controller list \
                     ({controllers:?}); it cannot manage it"
                )
            }
        }
        Err(_) => bail!(
            "{me} cannot read {canister}: it is almost certainly not a controller of it, \
             and only a controller can snapshot or load it. Add this identity as a \
             controller (its current controllers are public via `icp canister info \
             {canister}`), then retry."
        ),
    }
}

/// Loads a snapshot that is already on the target, without re-transferring it.
/// This is the second half of a staged migration: `migrate` leaves a verified
/// snapshot on the target, and this applies it. For a large canister that
/// saves streaming the whole thing a second time.
pub async fn load(
    agent: &Agent,
    me: Principal,
    target: Principal,
    source: Option<Principal>,
    snapshot: Option<Vec<u8>>,
    start: bool,
    delete_source: bool,
) -> Result<()> {
    ensure_control(agent, target, me).await?;
    // If we are going to delete the source, prove we can before touching the
    // target, so a missing controller surfaces early rather than after loading.
    if delete_source {
        let source = source.ok_or_else(|| anyhow::anyhow!("--delete-source needs --source"))?;
        ensure_control(agent, source, me).await?;
    }

    let snapshot = match snapshot {
        Some(id) => id,
        None => ic::sole_snapshot(agent, target).await?.id,
    };
    println!("loading snapshot {} into {target}", hex::encode(&snapshot));

    // Loading replaces code and memory; the canister must be stopped.
    ic::stop(agent, target).await?;
    ic::load_snapshot(agent, target, &snapshot).await?;
    println!("  loaded");

    // When the source is still around, confirm the loaded code matches it.
    if let Some(source) = source {
        let module = verify::module_hash_check(agent, source, target).await?;
        println!(
            "  [{}] {:<22} {}",
            if module.pass { "PASS" } else { "FAIL" },
            module.name,
            module.detail
        );
        if !module.pass {
            bail!("module hash mismatch after load; the source was left untouched");
        }
    }

    if start {
        ic::start(agent, target).await?;
        println!("  started the target");
    } else {
        println!("  left the target STOPPED. Start it once you have cut over.");
    }

    if delete_source {
        let source = source.ok_or_else(|| anyhow::anyhow!("--delete-source needs --source"))?;
        ensure_control(agent, source, me).await?;
        ic::stop(agent, source).await?;
        ic::delete(agent, source).await?;
        println!("  deleted the source");
    }

    Ok(())
}

/// Fails if the target already holds code or a snapshot, so a migration never
/// silently overwrites an occupied canister. `--force` skips this.
async fn ensure_target_empty(agent: &Agent, target: Principal) -> Result<()> {
    let (_, module) = ic::status(agent, target).await?;
    if let Some(hash) = module {
        bail!(
            "target {target} already has code installed (module hash {}). \
             Migrating would overwrite it. Pass --force to proceed.",
            hex::encode(hash)
        );
    }
    let snapshots = ic::list_snapshots(agent, target).await?;
    if !snapshots.is_empty() {
        bail!(
            "target {target} already has {} snapshot(s). Migrating would add \
             another and overwrite its state on load. Pass --force to proceed.",
            snapshots.len()
        );
    }
    Ok(())
}

/// Streams every section of the source snapshot into the target snapshot.
async fn transfer(
    agent: &Agent,
    source: Principal,
    source_snapshot: &[u8],
    target: Principal,
    target_snapshot: &[u8],
    sizes: [(Section, u64); 3],
) -> Result<()> {
    for (section, total) in sizes {
        let mut offset = 0u64;
        while offset < total {
            let size = ic::CHUNK.min(total - offset);
            let chunk = ic::read_chunk(agent, source, source_snapshot, section, offset, size).await?;
            let read = chunk.len() as u64;
            ic::upload_data(agent, target, target_snapshot, section, offset, chunk).await?;
            offset += read;
        }
        println!("    streamed {} ({total} bytes)", section.label());
    }
    Ok(())
}

pub async fn migrate(
    agent: &Agent,
    me: Principal,
    source: Principal,
    target: Principal,
    opts: &Options,
) -> Result<()> {
    println!("migrating {source} -> {target}");

    // The whole premise: this identity controls both ends.
    ensure_control(agent, source, me).await?;
    ensure_control(agent, target, me).await?;
    println!("  controller check: this identity controls both canisters");

    // Refuse to clobber a target that already holds something, unless forced.
    if !opts.force {
        ensure_target_empty(agent, target).await?;
        println!("  target is empty");
    }

    if opts.stop_source {
        ic::stop(agent, source).await?;
        println!("  stopped the source for a quiescent snapshot");
    }

    let src_snap = ic::take_snapshot(agent, source).await?;
    println!("  took source snapshot {}", hex::encode(&src_snap.id));
    let meta = ic::snapshot_metadata(agent, source, &src_snap.id).await?;

    let tgt_snap = ic::upload_metadata(
        agent,
        target,
        UploadShape {
            wasm_module_size: meta.wasm_module_size,
            globals: meta.globals.clone(),
            wasm_memory_size: meta.wasm_memory_size,
            stable_memory_size: meta.stable_memory_size,
            certified_data: &meta.certified_data,
        },
    )
    .await?;
    println!("  opened target snapshot {}", hex::encode(&tgt_snap));

    transfer(
        agent,
        source,
        &src_snap.id,
        target,
        &tgt_snap,
        [
            (Section::WasmModule, meta.wasm_module_size),
            (Section::WasmMemory, meta.wasm_memory_size),
            (Section::StableMemory, meta.stable_memory_size),
        ],
    )
    .await?;

    // Prove the transfer. This is the point of the tool, and it runs whether
    // or not the snapshot is loaded.
    println!("  verifying the transfer:");
    let report = verify::verify_snapshots(agent, source, &src_snap.id, target, &tgt_snap).await?;
    report.print();
    if !report.passed() {
        bail!("transfer verification failed, nothing was loaded or deleted");
    }

    // Non-destructive by default: stop here with the verified snapshot parked
    // on the target, and say how to finish. As a plain controller these are
    // direct calls; no operator proxy, which an outside party could not use.
    if !opts.load {
        println!(
            "\nstaged and verified. The target holds snapshot {} but has NOT been loaded.\n\
             Finish when ready without re-transferring:\n  \
             ce-migrate load --target {target} --source {source} --start\n\
             or by hand:\n  \
             icp canister stop {target} -n ic\n  \
             icp canister snapshot restore {target} {} -n ic\n  \
             icp canister start {target} -n ic",
            hex::encode(&tgt_snap),
            hex::encode(&tgt_snap),
        );
        if opts.stop_source {
            println!("The source is stopped. Restart it with `icp canister start {source} -n ic` to undo.");
        }
        return Ok(());
    }

    // Loading replaces the target's code and memory, so it must be stopped first.
    ic::stop(agent, target).await?;
    ic::load_snapshot(agent, target, &tgt_snap).await?;
    println!("  loaded the snapshot into the target");

    // Now the target runs the code, so confirm it is the same module.
    let module = verify::module_hash_check(agent, source, target).await?;
    println!(
        "  [{}] {:<22} {}",
        if module.pass { "PASS" } else { "FAIL" },
        module.name,
        module.detail
    );
    if !module.pass {
        bail!("module hash mismatch after load, the source was left untouched");
    }

    if opts.start {
        ic::start(agent, target).await?;
        println!("  started the target");
    } else {
        println!(
            "  loaded, left the target STOPPED. Cut over references and sweep any tokens, then:\n  \
             icp canister start {target} -n ic"
        );
    }

    if opts.delete_source {
        ic::stop(agent, source).await?;
        ic::delete(agent, source).await?;
        println!("  deleted the source");
    } else {
        println!(
            "  kept the source ({}). Move references and sweep tokens to {target}, then delete it.",
            if opts.stop_source { "stopped" } else { "running" }
        );
    }

    println!("done: {source} migrated to {target}");
    Ok(())
}
