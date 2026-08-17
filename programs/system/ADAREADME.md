# ADA — Account-Derived Accounts

**A protocol-level extension to Solana's account model, implemented in a fork of Agave's System Program.**

---

## 1. Motivation

Solana's account model has no native concept of ownership hierarchy. A program can create accounts, assign owners, and derive PDAs — but nothing in the runtime records *which account created which*. Every "this pool belongs to this protocol" or "this position belongs to this user" relationship today is enforced entirely by program logic and convention, not by the protocol itself.

That gap is the source of several recurring classes of exploit and operational cost across Solana DeFi:

- **Spoofed accounts.** An attacker crafts an account that superficially resembles a legitimate pool, oracle, or position. Nothing at the runtime level distinguishes it from the real thing — verification requires trusting a specific program's internal logic or an off-chain indexer.
- **Opaque protocol state.** Determining "does this account genuinely belong to this protocol" requires either auditing the spawning program's source, or querying `getProgramAccounts` with filters against an RPC node — both of which are trust-requiring or infrastructure-dependent.
- **Orphaned accounts.** Dead accounts accumulate on-chain indefinitely because there's no structural owner responsible for closing them, and no cheap way to prove an account is safe to reclaim.

ADA addresses this by adding a new primitive to the account model: **an account can derive and cryptographically prove ownership of child accounts**, with the parent/child relationship stored as on-chain data rather than implied by convention.

---

## 2. Design summary

ADA introduces one new account state type and four new System Program instructions:

| Component | Role |
|---|---|
| `DerivedAccountState` | Data stored inside every ADA-managed account: parent, depth, child count, revocation status, recovery authority |
| `InitializeRoot` | Creates a tree's root node |
| `DeriveAccount` | Spawns a cryptographically-verified child from an existing parent |
| `RevokeChildAccount` | Invalidates a specific child, authenticated by its parent |
| `ReclaimAccount` | Closes a revoked account and returns its lamports to a named recovery authority |

The address derivation scheme is deliberately analogous to Solana's existing PDA mechanism: a child's address is computed as `create_with_seed(parent, seed, owner)`, and the runtime re-derives and verifies this on every `DeriveAccount` call — exactly as it does for `CreateAccountWithSeed`. The difference is what the derivation *proves*: PDAs prove a program can sign for an address; ADA proves an account was legitimately spawned by a specific parent.

---

## 3. Files modified

| File | Crate | Contents |
|---|---|---|
| `system-interface/src/instruction.rs` | `solana-system-interface` | `SystemInstruction` enum — the four new variants |
| `programs/system/src/system_processor.rs` | `solana-system-program` | Instruction handlers, dispatch logic |
| `programs/system/src/derived_account.rs` | `solana-system-program` | `DerivedAccountState`, `verify_lineage` |

`solana-system-interface` was vendored locally into the workspace (`./system-interface`) rather than edited via the published crates.io dependency, since the workspace's `Cargo.toml` resolved it externally by default.

---

## 4. `DerivedAccountState`

```rust
pub struct DerivedAccountState {
    pub parent: Pubkey,
    pub depth: u8,
    pub children_count: u64,
    pub recovery_authority: Option<Pubkey>,
    pub revoked: bool,
}
```

| Field | Purpose |
|---|---|
| `parent` | The account that spawned this one. `Pubkey::default()` for a root. |
| `depth` | Distance from the root. Enforced to increment by exactly 1 per derivation, and checked during lineage verification. |
| `children_count` | Number of accounts this one has spawned. Incremented on every successful `DeriveAccount` call where this account is the parent. |
| `recovery_authority` | Optional pubkey authorized to reclaim this account's lamports after revocation. |
| `revoked` | Set by the parent via `RevokeChildAccount`. Once true, this account fails lineage verification and becomes eligible for reclamation. |

**Why ownership is never reassigned.** Solana's runtime permits only an account's owning program to write its data. Early implementation reassigned ownership to the caller-supplied `owner` parameter during account creation — this is incorrect: once ownership transfers away from System Program, System Program permanently loses write access to `DerivedAccountState`, and every subsequent operation fails with `ExternalAccountDataModified`. The fix — and the correct long-term design — keeps every ADA-managed account owned by `system_program::id()` indefinitely, exactly matching how nonce accounts track an internal "authority" separately from the account's actual owner field.

---

## 5. Instructions

### `InitializeRoot`

```rust
InitializeRoot {
    lamports: u64,
    space: u64,
    owner: Address,
    recovery_authority: Option<Address>,
}
```

**Accounts:** `[from (signer), to (signer)]`

Creates the first node in a tree. No parent to validate, no signature chain beyond the new account's own key. Writes `DerivedAccountState { parent: default, depth: 0, children_count: 0, revoked: false, .. }`.

### `DeriveAccount`

```rust
DeriveAccount {
    parent: Address,
    seed: String,
    lamports: u64,
    space: u64,
    owner: Address,
}
```

**Accounts:** `[from (signer), parent (signer), to]`

The core operation. Execution order:

1. Borrow the parent, confirm it is owned by System Program, deserialize its state, reject if `revoked`.
2. Confirm the parent's key is an actual transaction signer — a child cannot be spawned without its parent's explicit authorization.
3. Confirm the target address matches `create_with_seed(parent, seed, owner)` — this is the address-spoofing prevention. Any mismatch fails with `AddressWithSeedMismatch` before any state is touched.
4. Allocate the child's space (ownership stays with System Program), write its `DerivedAccountState` with `parent` set to the real parent and `depth` incremented by one.
5. Re-borrow the parent, increment `children_count`, write it back.
6. Transfer lamports from payer to the new child.

### `RevokeChildAccount`

**Accounts:** `[parent (signer), child]`

Confirms the parent signed, confirms the child's stored `parent` field genuinely matches the claimed parent (preventing a third party from revoking an account they have no relationship to), then sets `revoked = true`. Does not touch the parent's `children_count` — the child continues to exist, only its validity is affected.

### `ReclaimAccount`

**Accounts:** `[recovery_authority (signer), child, destination]`

Closes a revoked account and returns its lamports.

1. Confirm `recovery_authority` is an actual signer among the instruction's accounts.
2. Confirm the child is `revoked`.
3. Confirm the signer matches the child's stored `recovery_authority` field exactly.
4. Zero the child's data length, drain its lamports, credit them to `destination`.

**Design note.** The initial implementation checked `signers.contains(&recovery_authority)` without ever including `recovery_authority` among the instruction's own accounts — `signers` is derived strictly from accounts present in the instruction, so an authority not listed as an account can never appear in it. This was caught by a failing test (`MissingRequiredSignature` where `Ok(())` was expected) before being shipped. Fixed by adding `recovery_authority` as an explicit first account.

---

## 6. `verify_lineage`

```rust
pub fn verify_lineage(
    chain: &[(Pubkey, DerivedAccountState)],
    expected_root: &Pubkey,
) -> bool
```

A standalone verification function, independent of any instruction — no `InvokeContext`, no account borrowing, pure data in, boolean out.

Given a chain ordered from leaf to root, it verifies:

- No link in the chain is revoked
- Each account's stored `parent` correctly names the next account in the chain
- `depth` decreases by exactly 1 at each step (catching spliced or out-of-order chains that would otherwise pass a naive parent-pointer check)
- The final link either *is* the expected root, or correctly names it as parent

**Why it's decoupled from the instruction layer.** This is the function meant to answer "does this account genuinely belong to this protocol" — for a client, a wallet, an aggregator, or eventually another on-chain program. Keeping it free of Solana-specific machinery means it can be called anywhere the caller can fetch account data, without requiring a transaction.

---

## 7. What can currently be built on this

**Trustless protocol hierarchies.** A protocol calls `InitializeRoot` once, then `DeriveAccount` repeatedly to grow out pools, positions, or any domain-specific tree — each node's lineage is real on-chain data, not a naming convention.

**Spoofing-resistant account verification.** Because `DeriveAccount` enforces address derivation at the runtime level, no account can claim membership in a tree without the parent's cryptographic cooperation. A third party reading an account's `DerivedAccountState` and walking its lineage via `verify_lineage` gets a genuine trustless answer — no indexer, no trusting the spawning program's source code.

**Authenticated revocation.** A parent can invalidate a specific child, with the runtime enforcing that only the true parent (verified against the child's own stored data) can do so.

**Fund recovery from revoked accounts.** A named `recovery_authority`, set at creation time, can close a revoked account and reclaim its lamports — turning "this account is defunct" into an actual, executable cleanup path rather than permanently locked rent.

**Multi-level composition.** Depth tracking is correctly enforced across arbitrary tree depth — a position under a pool under a protocol root works today, not just single-parent/single-child relationships.

---

## 8. Known limitations

- **`verify_lineage` is not yet callable via CPI.** It exists as a library function usable by off-chain clients and tooling, not yet as something another on-chain program can invoke to gate its own instruction logic. This is the gap between "provable off-chain" and "enforced on-chain by third-party programs."
- **No cascading revocation.** Revoking a mid-tree node does not propagate a `revoked` flag onto its descendants' own stored state. `verify_lineage` still correctly rejects descendants of a revoked node (because it walks and checks every link), but the descendants' own on-chain data does not reflect this directly.
- **No permissionless sweep mechanism.** Reclamation currently requires a specific, pre-designated `recovery_authority`. There is no bounty-incentivized mechanism allowing any third party to prove an account is orphaned and claim its rent — the broader "network-wide orphan cleanup" vision discussed during design remains unbuilt.
- **`children_count` never decrements.** Closing or reclaiming a child does not update its former parent's count.

---

## 9. Test coverage

76 tests pass, comprising the full pre-existing System Program suite (69 tests, unmodified) plus:

| Test | Verifies |
|---|---|
| `test_initialize_root` | Root creation with correct default state |
| `test_derive_account` | Full happy path — signatures, derivation, lamport transfer, state on both parent and child |
| `test_derive_account_seed_mismatch_fails` | Address spoofing is rejected |
| `test_derive_account_parent_not_signed` | Derivation requires the parent's signature |
| `test_derive_account_revoked_parent_fails` | A revoked parent cannot spawn children |
| `test_revoke_child_account` | Revocation correctly updates and persists state |
| `test_reclaim_account` | Fund recovery to the correct, signing recovery authority |
| `test_verify_lineage_valid_chain` | Correct chains verify |
| `test_verify_lineage_revoked_link_fails` | A revoked link anywhere in the chain invalidates it |
| `test_verify_lineage_wrong_parent_fails` | Spliced or misclaimed parentage is rejected |
| `test_verify_lineage_wrong_root_fails` | Chains not actually rooted at the expected account are rejected |
| `test_verify_lineage_empty_chain_fails` | Degenerate input is rejected, not silently accepted |
