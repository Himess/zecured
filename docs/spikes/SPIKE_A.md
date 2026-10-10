# Spike A — 3-of-5 FROST vault in Ironwood (result: ✅ GO)

Date: 2026-10-11 (plan Day 1; gate 1 was scheduled for 2026-10-13).

## Success criterion (PLAN § 18)

> A two-output transaction leaving the vault is confirmed on testnet.

**Met.** Zcash testnet, block **4,486,491**:

| | |
|---|---|
| Deposit (faucet → vault A) | `1126d123eb59bbb1a359f3ffe7a12c92e2dc2ce6e305c5332643cd1085d084ad`, block 4,486,485, v6, 2 Ironwood actions, nothing else (would pass P1's structure check) |
| Vault spend | `a370144b91400634ac849c04841b54b12043480d7a993c606d11fb666eb6556d`, block 4,486,491, v6, NU7 branch `0x77190AD9`, 3 Ironwood actions, no transparent / Sapling / Orchard parts |
| Outputs | 0.04 TAZ → vault B (vault OVK); 0.03 TAZ → vault C (`ovk = None`); 0.05485 TAZ change → vault A internal address; fee 0.00015 TAZ (ZIP 317, 3 logical actions) |
| Authorization | FROST(Pallas, BLAKE2b-512) re-randomized, signers 1, 2, 4 of 5, randomizer = the PCZT spend's `alpha` |
| Proof | Halo 2 (post-NU6.3 circuit), 14.6 s on an M2 (proving key build included) |

Every recipient was confirmed by scanning: vault B and C each see exactly their note; vault
A sees its deposit spent at 4,486,491 and the change note (internal scope).

## What is proven

1. **DKG**: 5-party FROST DKG produces an even-ỹ group key usable as Orchard `ak`.
2. **ZIP 2005 keys**: vault FVK derived with `use_qsk = true`; matches 8 independent vectors
   (frost-tools PR #610) for `nk`, `qsk`, `qk`, `rivk_ext`, FVK, default address and UFVK.
3. **Addresses**: Ironwood-only UA = UA with only the Orchard-protocol receiver (`utest1…`);
   wallets/faucets pay it in the Ironwood pool.
4. **Scanning**: compact blocks (`ironwood_actions`), external + internal IVK, notes keyed by
   (height, tx index, action index); our Ironwood tree root equals the server's at every
   checked height (21,436 blocks since NU7 in 20.6 s).
5. **Spending**: zcash_primitives builder → PCZT (Creator, IoFinalizer, Prover) → FROST
   signature over the shielded sighash with `alpha` → `apply_ironwood_signature` →
   TransactionExtractor (verifies proof + signatures) → lightwalletd `SendTransaction`.
6. **Privacy properties** (offline test `frost_spend`): the vault's external OVK recovers only
   the borrower payout, the internal OVK only the change, nothing recovers the `ovk = None`
   third-party output; an impostor key's signature is rejected.

## Commands

```bash
cd signer
cargo test -p zecured-threshold
cargo test -p zecured-zcash-ops            # includes the offline FROST spend (~50 s)
./target/debug/zecured-spike --lwd https://testnet.zec.rocks:443 info
./target/debug/zecured-spike --lwd https://testnet.zec.rocks:443 dkg
./target/debug/zecured-spike --lwd https://testnet.zec.rocks:443 scan
./target/debug/zecured-spike --lwd https://testnet.zec.rocks:443 txshape <txid>
./target/debug/zecured-spike --lwd https://testnet.zec.rocks:443 spend --to <ua> --amount <zat> --third <ua> --third-amount <zat>
```

## Gotchas

- Only the `-pre` crate wave knows NU7 on testnet (D1). zcash-devtool / frost-tools / zingo
  cannot transact on testnet today.
- `reddsa` 0.6 dropped FROST → reddsa 0.5.2 with pasta_curves 0.5, bridged by bytes (D2).
- `alpha` is not exposed on the serialized PCZT; read it through
  `low_level_signer::Signer::sign_ironwood_with` (parsed bundle).
- The deprecated `frost_rerandomized::sign` is the only public way to sign with an explicit
  randomizer (frost#1094 may un-deprecate it).
- Dependencies must be optimized even in dev builds (`[profile.dev.package."*"] opt-level = 3`),
  otherwise proving and trial decryption are unusably slow.
- One public post-NU7 light wallet server exists (`testnet.zec.rocks`); keep the regtest
  fallback in mind (D9).

## Not covered yet (by design, later days)

Networked nodes and coordinator, persistent key storage, policy P0–P6, payout queue, P1
classification with confirmations and expiry height, Base integration.
