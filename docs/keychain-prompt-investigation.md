# Keychain prompt on every dev rebuild — investigation notes

## Problem

On macOS Sonoma and Sequoia, every fresh `cargo build` / `dx serve` of `linewise-desktop` triggers a system Keychain prompt asking for the user's login password before the app can read its `linewise-desktop / refresh_token` entry. The prompt is blocking — the user must type their password (or click "Always Allow") on every rebuild — which makes the inner dev loop painful.

The entry is created by the `keyring` crate at [crates/lw-core/src/auth/mod.rs](../crates/lw-core/src/auth/mod.rs) under service `linewise-desktop` and account `refresh_token`.

## Root cause (confirmed)

macOS uses a **partition list** as the access gate on keychain items, separate from the classic application ACL. When a generic-password item is created, macOS attaches a partition-list entry of the form `cdhash:<hex>` that pins the *current* binary's code-directory hash. A read by a different binary — or by the same logical binary after a rebuild changed its cdhash — fails the partition check and the OS prompts.

`cargo build` produces an ad-hoc signature whose cdhash is a hash over the binary contents. Every rebuild changes the binary (build timestamps, rustc-emitted metadata, debug info), so every rebuild has a fresh cdhash that does not match the partition list. Hence the prompt fires.

Clicking "Always Allow" updates the *application ACL* with a new cdhash entry but **does not** update the partition list. The partition list remains pinned to whichever cdhash was current when the user first authorized. So "Always Allow" appears to work for one rebuild and then stops.

## What we tried that did not work

These are dead ends, recorded so we don't relitigate them.

### Self-signed code-signing identity (`Linewise Dev (self-signed)`)

The theory: a stable certificate hash means a stable designated requirement, so the application ACL would match across rebuilds.

The implementation: an xtask `dev-codesign` subcommand that ran `openssl req -x509`, packaged the cert into a PKCS12 with `-legacy` (OpenSSL 3.x default rejects macOS's PKCS12 reader), imported into the login keychain, marked it trusted for `codeSign` via `add-trusted-cert`. Plus a `dev-entitlements.plist` file checked into the repo so `dx serve --codesign --apple-team-id "Linewise Dev (self-signed)" --apple-entitlements …` could finish its build pipeline.

**The signing worked.** `codesign -dvvv` showed `Authority=Linewise Dev (self-signed)`, the designated requirement was the stable cert hash, and `dx serve --codesign` produced a signed bundle on every rebuild.

**The keychain still prompted.** The application ACL on the keychain item correctly listed our app with the right designated requirement (`identifier "com.example.LinewiseDesktop" and certificate leaf = H"01b29b…"`), but the partition list still pinned the per-rebuild cdhash and macOS prompted on every build.

### Widening the partition list

The theory: `security set-generic-password-partition-list -S "apple-tool:,apple:,unsigned:"` would replace the cdhash pin with a broader allowlist, and our self-signed binary would slot into one of those buckets.

**The write succeeded** (`security dump-keychain` confirmed the partition_id description changed to `apple-tool:, apple:, unsigned:`).

**But macOS rewrote it back.** On the next authorization, the partition list reverted to `cdhash:<new-hash>`. The partition list is not a persistent allowlist — it functions as a single-entry recency cache that macOS rewrites after each successful authorization. There is no documented way to make it stick on a self-signed cert without a Team ID.

### Empty partition list

We considered `security set-generic-password-partition-list -S ""` to disable the partition check. We did not run it interactively, but it would have hit the same rewrite-on-authorization behavior; the partition list would just re-pin to the next cdhash. The escape hatch that makes the partition gate a no-op only fires when the requesting cert has a Team ID that is already on the list, which a self-signed cert lacks.

### Summary of dead ends

The throughline: the partition gate on macOS Sonoma+ requires either a `cdhash:<exact-hash>` (which changes every rebuild) or a `teamid:<TEAMID>` (which a self-signed cert does not have). Without a real Apple Developer Program membership, there is no certificate-based partition entry that survives a rebuild.

## Decision: defer to Option B

We accept the prompt for now and plan to fix it properly with **Option B — a real Apple Developer Program "Developer ID Application" certificate** when we ship to external users.

Why Option B and not a file-based keyring as a dev workaround:

1. We need notarization for external distribution anyway. Notarization requires an Apple Developer Program membership ($99/year). The same account issues the Developer ID Application cert that fixes this problem for free as a side effect.
2. A file-based dev keyring would be a parallel code path that diverges from the production code. Two implementations of "where do we store the refresh token" is more code to maintain than the prompt itself costs us during dev.
3. The dev-loop prompt is annoying but not blocking — clicking "Allow" once per rebuild is a one-second cost. Engineering time spent on a workaround pays back slowly.

When the Developer ID cert lands, the recipe is:

1. Buy / enroll an Apple Developer Program membership.
2. Generate a "Developer ID Application" certificate via Xcode or the developer portal. Install the private key and cert in the developer's login keychain.
3. Sign with the Developer ID identity. The cert has a Team ID; the macOS keychain partition gate will accept `teamid:<TEAMID>` as a stable allowlist entry; rebuilds with the same cert keep matching, and the prompt does not fire.
4. Add `[bundle.macos] signing_identity = "Developer ID Application: <Org Name> (<TEAMID>)"` to `Dioxus.toml` so `dx bundle` picks it up automatically. `dx serve --codesign --apple-team-id "Apple Development: <name> (<TEAMID>)"` works for the dev loop with no further setup.
5. Wire notarization into the release pipeline (`xcrun notarytool submit`).

## Files removed in this cleanup

- `crates/xtask/src/dev_codesign.rs` (the self-signed xtask module)
- The `DevCodesign` subcommand in `crates/xtask/src/main.rs`
- `dev-entitlements.plist` at the repo root
- The `scripts/dev-codesign.sh` helper that predated the xtask migration

These all assumed the self-signed cert path that we have now ruled out. Keeping them in the codebase would invite a future contributor to retry the same dead end.

## Useful diagnostic commands (if this comes back)

```sh
# Inspect the ACL + partition list on the keychain entry
security dump-keychain -a ~/Library/Keychains/login.keychain-db \
  | awk '/svce.*linewise-desktop/,/^$/'

# Show a signed bundle's authority and designated requirement
codesign -dvvv path/to/LinewiseDesktop.app
codesign -d --requirements - path/to/LinewiseDesktop.app

# Wipe the keychain entry to start fresh
security delete-generic-password \
  -s linewise-desktop -a refresh_token \
  ~/Library/Keychains/login.keychain-db
```

## References

- macOS Keychain partition list, official commentary: <https://developer.apple.com/library/archive/technotes/tn2206/_index.html>
- `security(1)` man page on `set-generic-password-partition-list`
- The `keyring` crate's [macOS implementation](https://docs.rs/keyring/latest/keyring/macos/) — uses `SecKeychainAddGenericPassword`, no explicit `SecAccess` setup, hence the empty ACL and default partition pinning we observed
