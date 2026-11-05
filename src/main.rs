//! ce-migrate: move a canister's state onto a cloud engine, and prove it
//! landed byte-for-byte.
//!
//! The identity this runs as must be a controller of both the source and the
//! target canister. Everything else (creating the target through the engine
//! operator, moving references afterward) is out of scope and lives in
//! MANUAL.md.

mod ic;
mod migrate;
mod verify;

use std::path::PathBuf;

use anyhow::{Context, Result};
use candid::Principal;
use clap::{Parser, Subcommand};
use ic_agent::identity::{BasicIdentity, Prime256v1Identity, Secp256k1Identity};
use ic_agent::{Agent, Identity};

#[derive(Parser)]
#[command(name = "ce-migrate", about = "Migrate canister state onto a cloud engine and verify it.")]
struct Cli {
    /// The replica to talk to.
    #[arg(long, default_value = "https://icp-api.io", global = true)]
    host: String,

    /// PEM file of the identity to sign as. Export it with `icp identity
    /// export`. Must control both canisters. Omitting it uses the anonymous
    /// identity, which can only read.
    #[arg(long, global = true)]
    identity: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stop the source, snapshot it, stream it into the target, and verify.
    /// Non-destructive by default: it does not load the target or delete the
    /// source unless asked.
    Migrate {
        /// Source canister, the one being moved.
        #[arg(long)]
        source: Principal,
        /// Target canister on the engine, already created and controlled by
        /// this identity.
        #[arg(long)]
        target: Principal,
        /// Do not stop the source before snapshotting. By default it is
        /// stopped so no state is created after the snapshot. The snapshot is
        /// consistent either way, and --no-stop just risks losing post-snapshot
        /// writes. Safe only for an idle or read-only canister.
        #[arg(long)]
        no_stop: bool,
        /// After verifying, load the snapshot into the target. The target is
        /// left stopped. Off by default: the snapshot is staged and verified
        /// only.
        #[arg(long)]
        load: bool,
        /// Start the target after loading. Requires --load. Off by default.
        #[arg(long)]
        start: bool,
        /// Delete the source once the target is loaded and verified. Requires
        /// --load. Off by default.
        #[arg(long)]
        delete_source: bool,
        /// Proceed even if the target already has code or snapshots. Off by
        /// default; migrating into a non-empty canister is refused.
        #[arg(long)]
        force: bool,
        /// A principal allowed to differ between the source and target
        /// controller sets. Repeat for several. The target is otherwise held
        /// to the same controllers as the source.
        #[arg(long = "accept-controller")]
        accept_controller: Vec<Principal>,
    },
    /// Load a snapshot that is already staged on the target, without
    /// re-transferring it. Finishes a `migrate` run, so a large canister moves
    /// once. The target is left stopped unless --start.
    Load {
        /// Target canister holding the staged snapshot.
        #[arg(long)]
        target: Principal,
        /// Source canister. Optional, and used only to re-check that the loaded
        /// module matches it. Pass it if the source still exists.
        #[arg(long)]
        source: Option<Principal>,
        /// Snapshot id (hex) to load. Defaults to the target's only snapshot.
        #[arg(long)]
        snapshot: Option<String>,
        /// Start the target after loading. Off by default.
        #[arg(long)]
        start: bool,
        /// Delete the source after loading. Requires --source. Off by default.
        #[arg(long)]
        delete_source: bool,
        /// A principal allowed to differ between the source and target
        /// controller sets, checked only when --source is given. Repeat for
        /// several.
        #[arg(long = "accept-controller")]
        accept_controller: Vec<Principal>,
    },
    /// Compare an existing source snapshot against a target snapshot, and the
    /// two live module hashes. Reads only.
    Verify {
        #[arg(long)]
        source: Principal,
        #[arg(long)]
        target: Principal,
        /// Source snapshot id (hex). Defaults to the source's only snapshot.
        #[arg(long)]
        source_snapshot: Option<String>,
        /// Target snapshot id (hex). Defaults to the target's only snapshot.
        #[arg(long)]
        target_snapshot: Option<String>,
    },
}

fn load_identity(path: &PathBuf) -> Result<Box<dyn Identity>> {
    let pem = std::fs::read_to_string(path)
        .with_context(|| format!("reading identity pem {}", path.display()))?;
    use k256::pkcs8::DecodePrivateKey;

    // Ed25519, as a PKCS8 "PRIVATE KEY".
    if let Ok(id) = BasicIdentity::from_pem(pem.as_bytes()) {
        return Ok(Box::new(id));
    }
    // Secp256k1 or P-256 as SEC1 "EC PRIVATE KEY".
    if let Ok(id) = Secp256k1Identity::from_pem(pem.as_bytes()) {
        return Ok(Box::new(id));
    }
    if let Ok(id) = Prime256v1Identity::from_pem(pem.as_bytes()) {
        return Ok(Box::new(id));
    }
    // What `icp identity export` actually writes: the key in PKCS8, whose label
    // is a plain "PRIVATE KEY" that the SEC1 loaders above skip. icp's default
    // key is secp256k1, so try that first, then P-256.
    if let Ok(key) = k256::SecretKey::from_pkcs8_pem(&pem) {
        return Ok(Box::new(Secp256k1Identity::from_private_key(key)));
    }
    if let Ok(key) = p256::SecretKey::from_pkcs8_pem(&pem) {
        return Ok(Box::new(Prime256v1Identity::from_private_key(key)));
    }
    anyhow::bail!(
        "could not load {} as an ed25519, secp256k1, or p256 key",
        path.display()
    )
}

async fn build_agent(host: &str, identity: &Option<PathBuf>) -> Result<(Agent, Principal)> {
    let mut builder = Agent::builder().with_url(host);
    let me = match identity {
        Some(path) => {
            let id = load_identity(path)?;
            let principal = id.sender().map_err(|e| anyhow::anyhow!(e))?;
            builder = builder.with_boxed_identity(id);
            principal
        }
        None => Principal::anonymous(),
    };
    let agent = builder.build().context("building the agent")?;
    // Mainnet's root key is baked in, so only fetch it off-mainnet.
    if !host.contains("icp-api.io") && !host.contains("ic0.app") {
        agent.fetch_root_key().await.context("fetching root key")?;
    }
    Ok((agent, me))
}

fn snapshot_id(hex: &str) -> Result<Vec<u8>> {
    hex::decode(hex).context("snapshot id must be hex")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let (agent, me) = build_agent(&cli.host, &cli.identity).await?;

    match cli.command {
        Command::Migrate {
            source,
            target,
            no_stop,
            load,
            start,
            delete_source,
            force,
            accept_controller,
        } => {
            if (start || delete_source) && !load {
                anyhow::bail!("--start and --delete-source require --load");
            }
            migrate::migrate(
                &agent,
                me,
                source,
                target,
                &migrate::Options {
                    stop_source: !no_stop,
                    load,
                    start,
                    delete_source,
                    force,
                    accept: accept_controller,
                },
            )
            .await?;
        }
        Command::Load {
            target,
            source,
            snapshot,
            start,
            delete_source,
            accept_controller,
        } => {
            if delete_source && source.is_none() {
                anyhow::bail!("--delete-source requires --source");
            }
            let snapshot = match snapshot {
                Some(h) => Some(snapshot_id(&h)?),
                None => None,
            };
            migrate::load(
                &agent,
                me,
                target,
                source,
                snapshot,
                start,
                delete_source,
                &accept_controller,
            )
            .await?;
        }
        Command::Verify {
            source,
            target,
            source_snapshot,
            target_snapshot,
        } => {
            let src = match source_snapshot {
                Some(h) => snapshot_id(&h)?,
                None => ic::sole_snapshot(&agent, source).await?.id,
            };
            let tgt = match target_snapshot {
                Some(h) => snapshot_id(&h)?,
                None => ic::sole_snapshot(&agent, target).await?.id,
            };
            let mut report = verify::verify_snapshots(&agent, source, &src, target, &tgt).await?;
            report.checks.push(verify::module_hash_check(&agent, source, target).await?);
            report.print();
            if !report.passed() {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
