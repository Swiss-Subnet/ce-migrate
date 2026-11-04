# Provisioning a migration (provider side)

For Swiss Subnet, not the tenant. This is the prep you do so a third party can
migrate their canister onto one of your engines with `ce-migrate` and no engine
access of their own. The tenant's side is [README.md](./README.md) and
[MANUAL.md](./MANUAL.md); they never see any of the below.

Names:

- `SRC` — the tenant's canister on the Swiss Subnet.
- `TGT` — the empty canister you create for them on the engine.
- `OP` — the engine's operator canister, from the engine's `engine_operator_id`
  (e.g. `cqpik-daaaa-aaacs-qaaaa-cai` for engine 152).
- `TENANT` — the principal the tenant will run `ce-migrate` as.

## What only you can do

Two things need engine privilege, and both are yours:

1. Creating `TGT` on the engine. Only the operator can make a canister on a
   cloud-engine subnet, so this is the one `--proxy` call in the whole flow.
2. Reserving and funding the engine in the first place.

Everything after that is a plain controller action the tenant does directly.

## 1. Create the target, controlled by the tenant

Sign as an engine member (one with CreateCanister permission on the engine):

```sh
icp canister create --detached --proxy OP --controller TENANT -n ic
# -> the new canister id. This is TGT.
```

The operator is also a controller of anything it creates, which is fine.

## 2. Make the tenant a controller of the source

Sign as `SRC`'s current controller:

```sh
icp canister settings update SRC --add-controller TENANT -n ic
```

## 3. Hand over

Give the tenant `SRC`, `TGT`, and nothing else. They run:

```sh
ce-migrate --identity their.pem migrate --source SRC --target TGT
```

and, once the verify block is all PASS, finish with the `load` subcommand,
which applies the staged snapshot without transferring it again:

```sh
ce-migrate --identity their.pem load --target TGT --source SRC --start
```

## Verify as a non-party (the trust check)

Before trusting this flow, prove it works for an identity that is only a
controller, never an engine admin. An admin would succeed through a different
authorization path, so testing as one would not prove the tenant case.

```sh
# A throwaway identity standing in for the tenant.
icp identity new tenant-test
NP=$(icp identity principal --identity tenant-test)
icp identity export tenant-test > tenant-test.pem

# Wrong on purpose: it controls nothing yet. Expect a clear "not a controller"
# refusal, not a raw rejection.
ce-migrate --identity tenant-test.pem migrate --source SRC --target SRC

# Provision as above: add NP to SRC (as SRC's controller), and
# icp canister create --detached --proxy OP --controller $NP -n ic   # -> TGT

# Now it controls both and never touches the operator again.
ce-migrate --identity tenant-test.pem migrate --source SRC --target TGT
ce-migrate --identity tenant-test.pem load --target TGT --source SRC --start
```

## Notes

- The operator relays only a whitelist of management calls (create, install,
  snapshots), so the tenant's other calls (stop, start, status, uninstall,
  delete) go direct as a controller, which works because they are a controller.
- `ce-migrate` refuses a target that already has code or snapshots unless
  `--force`, so a fresh `TGT` is the clean case. Create it empty and leave it.
