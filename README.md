# ce-migrate

Move a canister's state onto a cloud engine, and prove it landed byte-for-byte.

A cloud-engine subnet cannot receive a canister id: the NNS migration canister
rejects any migration touching a `CloudEngine` subnet. So you recreate the
canister on the engine and move its **state**. Every migrated canister gets a
**new id**. This tool does the state move and, more importantly, verifies it.

## What it verifies

A module hash only covers the wasm code. It says nothing about the memory. This
tool hashes the whole snapshot on both ends and compares:

- wasm module size and SHA-256
- wasm (heap) memory size and SHA-256
- **stable memory size and SHA-256**
- exported globals, `certified_data`, and the wasm chunk-store hashes
- the live module hash on both canisters (after a load)

If every check passes, the target holds the same state as the source, down to
the byte.

## The one assumption

Your provider gives you two canisters, the source you already run and an empty
target on the engine, and makes you a **controller of both**. That is all the
tool needs. It checks control first and stops if it is missing. You never touch
the engine, its operator, or any `--proxy`, because every call is an ordinary
management-canister call that routes to each canister's own subnet.

The by-hand version of the same migration is in [MANUAL.md](./MANUAL.md).
Provisioning the two canisters is the provider's job, in
[OPERATOR.md](./OPERATOR.md).

## Build

```sh
nix develop            # cargo, rustc, and icp in scope
cargo build --release  # writes target/release/ce-migrate

# or, once Cargo.lock exists:
nix build              # writes result/bin/ce-migrate
```

The first `cargo build` creates `Cargo.lock`, which `nix build` then pins
against.

## Use

Export the controller identity to a PEM once:

```sh
icp identity export > controller.pem   # keep this safe, it is a private key
```

**Stage and verify (the default, non-destructive):** stops the source,
snapshots it, streams it into the target, verifies, and stops. Nothing on the
target is loaded, nothing is deleted.

```sh
ce-migrate --identity controller.pem \
  migrate --source <source-canister> --target <engine-canister>
```

**Load it:** apply the snapshot that `migrate` already staged on the target,
without transferring it again. The target is left **stopped** on purpose, so it
does not run its timers against stale state before you cut over.

```sh
ce-migrate --identity controller.pem \
  load --target <engine-canister> --source <source-canister>
```

`--source` is optional here, used only to re-check that the loaded module
matches the source. Add `--start` to also start the target, and
`--delete-source` to remove the source afterward (which needs `--source`).
Normally you load, cut over references, sweep any tokens, start, confirm it is
live, and only then delete the source.

Staging and loading are split so a large canister streams **once**: `migrate`
uploads and verifies, then `load` applies that verified snapshot. If you would
rather do it in a single command, `migrate ... --load --start` stages, verifies,
loads, and starts in one pass (it also uploads only once, since it loads the
snapshot it just made). `--no-stop` leaves the source running while snapshotting,
which risks an inconsistent copy.

**Verify only**, against snapshots that already exist on both canisters:

```sh
ce-migrate --identity controller.pem \
  verify --source <source-canister> --target <engine-canister>
```

Snapshot ids default to each canister's sole snapshot. Pass
`--source-snapshot` / `--target-snapshot` (hex) when there is more than one.

## What it does not do

- **Preserve the canister id.** Impossible across subnets, so a migrated canister
  is a new principal. Everything that references the old id has to be repointed.
- **Create the target**, which your provider does for you before you start.
- **Move cycles.** On a cloud engine, canisters run free (the subnet is on a
  free cost schedule), so the node rent is the whole cost. Mind the subnet's
  state limit (~100 GiB per engine) as the ceiling on everything you migrate
  onto it.
- **Move tokens the canister holds.** A snapshot captures memory, not ledger
  balances. Any ICP or ICRC tokens the canister custodies live in the ledger
  canister keyed to the **old principal**, and the new canister cannot touch
  them. Sweep every balance out of the source before you delete it, or it is
  gone for good. The new principal also changes every deposit address the app
  derives from its own id, so funds sent to the old address after cutover are
  lost. Handle asset-custodying canisters by hand, which this tool will not do.

**Read that last point twice before `--delete-source` on anything holding ICP.**

Prove the round-trip on a throwaway canister before moving anything that
matters: snapshot download and upload is chunked, and a large stable memory is
a long transfer.
