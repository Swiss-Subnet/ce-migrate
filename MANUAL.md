# Manual migration with `icp`

The same migration by hand, so you can see every step the tool automates and
run it when you would rather not use the tool. Only creating the target needs
the operator, through `--proxy <operator>`, because only it can make a canister
on the engine's subnet. Every other call is a plain controller call that routes
to the target's subnet on its own, so it carries no `--proxy`.

Names used below:

- `SRC` — your canister on the Swiss Subnet, the one being moved.
- `TGT` — the empty canister on the engine your provider gave you.
- `-n ic` targets mainnet. You sign as your own identity, which controls both.

You never pass `--proxy`. Your provider created `TGT` for you, so every command
here is a plain controller call that routes to the right subnet on its own.

## 1. Snapshot the source

Stop it first. The snapshot is a consistent atomic copy even while running, so
this is not about the copy: it freezes the source so no state is created after
the snapshot that the migration would miss.

```sh
icp canister stop SRC -n ic
icp canister snapshot create SRC -n ic
# -> prints the source snapshot id, call it SSID
```

## 3. Move the snapshot to the target

Download it, then upload it to the target. These are plain controller calls, so
no `--proxy`: only `create` in step 1 needs the operator, and a non-party could
not use the proxy anyway.

```sh
icp canister snapshot download SRC SSID --output ./snap -n ic
icp canister snapshot upload TGT --input ./snap -n ic
# -> prints the target snapshot id, call it TSID
```

## 4. Verify the transfer before loading

Compare the two snapshots and the live module hashes. The `verify` subcommand of
the tool does this against the live canisters, which is stronger than comparing
local files:

```sh
ce-migrate --identity controller.pem \
  verify --source SRC --target TGT \
  --source-snapshot SSID --target-snapshot TSID
```

By hand, at minimum compare the module hash and the snapshot sizes:

```sh
icp canister status SRC --json -n ic | grep -i module_hash
icp canister status TGT --json -n ic | grep -i module_hash
```

A matching module hash only tells you the **code** matches. It says nothing
about stable memory. Use the tool for the real check.

## 5. Load it into the target

Loading replaces the target's code and memory, so stop it first:

```sh
icp canister stop TGT -n ic
icp canister snapshot restore TGT TSID -n ic
icp canister start TGT -n ic
```

## 6. Confirm, then cut over

```sh
icp canister status TGT --json -n ic | grep -i module_hash
```

Repoint everything that referenced `SRC` to `TGT`: inter-canister callers,
frontend configs, custom domains, and the engine's
`/.well-known/ic-architecture`. There is no redirect from the old id.

## 7. Sweep tokens, then clean up

A snapshot moves memory, not ledger balances. Any ICP or ICRC tokens `SRC`
custodies are keyed to its principal and do **not** follow the state to `TGT`.
Before deleting `SRC`, transfer every balance it holds to `TGT` (or elsewhere),
and update any deposit address the app derives from its own principal. Deleting
`SRC` while it still holds ICP loses that ICP: no canister will exist to sign it
out.

Keep `SRC` stopped until the cutover is proven and swept. Then, when you are
sure:

```sh
icp canister delete SRC -n ic
```

## Notes

- A migrated canister always gets a **new id**. The NNS migration canister
  refuses id-preserving migration onto a cloud engine.
- Canisters on the engine run free (the subnet is on a free cost schedule), so
  there is nothing to top up per canister, the node rent is the cost.
- The engine subnet has a state ceiling (~100 GiB, less the replica's ~10 GiB
  reserve). That is the total across everything you migrate onto one engine.
- Snapshot download and upload are chunked. A large stable memory is a long
  transfer. Rehearse on a throwaway canister first.
