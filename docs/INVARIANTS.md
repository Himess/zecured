# Invariants

What must always hold in Zecured, where the code enforces it, and which tests check it. I1 to I10 date
from the start of the project, I11 to I14 from the first security review, I15 to I17 from Tier 3
(docs/TIER3.md), I18 to I31 from the contract round v3 (docs/DECISIONS.md D40 to D54, design items
T-01 to T-09). Contract tests live in `contracts/test`; the invariant campaign is
`test/invariant/ZecuredInvariants.t.sol` (random sequences of deposits, gates, exits, payouts,
disputes and attacks against the real Morpho Blue on a Base Sepolia fork); the symbolic checks are
in `test/halmos`.

| # | Invariant | Enforced by | Checked by |
|---|---|---|---|
| I1 | zZEC total supply equals the sum of `outstanding` over active vaults | The Registry mints only when the Intake activates a vault and burns on every exit; zZEC lets only the Registry mint or burn | invariant `I1`; Halmos `check_I1_everyExitBurnsExactlyWhatItsClaimsRelease`, `check_K3_onlyActivationMints`, `ZZECGuard` |
| I2 | zZEC moves only between Morpho, the Registry and the SettlementAdapter; no account holds it | zZEC's transfer guard: an armed, one-shot expectation for every transfer out of Morpho | invariant `I2`; Halmos `ZZECGuard` (5 checks); `TransferGuard.t.sol` |
| I3 | No ZEC of a vault leaves it before its zZEC is burned and a claim exists (refunds are claims too) | The Registry creates the claim in the transaction that burns; the signers pay only committed claims (Tier 3: commitment before signature shares) | invariant `I3_I9_I11_I12`; node policy tests |
| I4 | One account, one vault | `_vaultOf` in the Registry, checked in every deposit hook | invariant `I4_I5` |
| I5 | A deposit tag and a vault id are each used at most once | `_addLineage` and `reserveDeposit` | invariants `I4_I5`, `I13_I20`; attack `attackDoubleLanding` |
| I6 | Fewer than three distinct signer signatures are always rejected (five for `VaultOpened` and for a new verifier version) | One sorted, strictly increasing recovery loop in the Registry, the spend logic, the Intake and the version registry | Halmos `check_I6_onlyEnoughDistinctSignersPass`, `check_I6_reimburseDestinationNeedsThreeDistinctSigners`; unit tests |
| I7 | A close burns exactly the outstanding and splits it exactly into the user's and the market maker's claims | `recordClose` | invariant `I7`; Halmos `check_I7_closeSplitsExactlyTheOutstanding` |
| I8 | The SettlementAdapter never withdraws collateral without a valid, unexpired, uncancelled request signed by the account, executed by its named executor | SettlementAdapter | SettlementAdapter fork tests; attacks `attackCloseRequest`, `attackDeleverage` |
| I9 | Seized, withdrawn or retired zZEC never exceeds the vault's outstanding | `recordLiquidation`, `retireVault` | invariant `I3_I9_I11_I12`; Halmos `check_I9_liquidationSeizesAtMostTheOutstanding` |
| I10 | (Off-chain) a vault's notes hold its outstanding, its unpaid claims and its unrefunded non-conforming notes | Signer nodes' ledger (`signer/crates/policy`) | policy crate tests; Base side: I15 |
| I11 | Everything that landed on a vault (its activating deposit and every refunded deposit) equals its outstanding plus all its claims, of every kind and status | Registry bookkeeping | invariant `I3_I9_I11_I12` |
| I12 | Morpho's collateral for an activated vault's account equals its outstanding | Every exit withdraws through the Registry and checks the amount received | invariant `I3_I9_I11_I12` |
| I13 | Every note the Intake accepted lands exactly once: a pending reservation, an activation or a refund; a reservation a reorganisation dropped frees its tag | Intake and Registry deposit hooks | invariant `I13_I20` |
| I14 | Cancelled if and only if never activated; a cancelled vault has no outstanding and only Refund claims | `openRefund`, `convertToRefund`, `releaseDeposit` | invariant `I14` |
| I15 | A vault's lineage notes that no committed payout spent hold exactly its outstanding, its pending activation and its uncommitted pending or written-off claims | Spend commitments: conservation and claim binding (`ZecuredSpendLogic`) | invariant `I15` |
| I16 | Every claim confirmed paid (or disputed) was committed against a lineage note of its own vault | `confirmPayout` requires the claim's input; commitments check the input's lineage | invariant `I16_I21` |
| I17 | `openVaultCount` counts exactly the vaults that are pending or active, or closed or cancelled with a claim pending or disputed | `_newClaim`, `_closeClaim`, `_settle`, disputes | invariant `I17` |
| I18 | A deposit is reserved only within the coverage cap: locked plus pending zatoshi within `maxTotalZat` and, at the high price, within `coverageBps` of the effective bond (`min(sum, 5/3 x the three smallest bonds)`) | Intake `_capRoom`; SignerBondV2 `effectiveBond` | invariant `I18_I22_I23_I24`; `IntakeGates.t.sol`; Halmos `check_effectiveBondNeverExceedsTheSum`, `check_cheapestThreeAreTheThreeSmallest` |
| I19 | The bond holds exactly the bonds, the unbondings and the unclaimed bounties; the compensator holds exactly its unassigned funds and what it set aside | SignerBondV2, ZecuredCompensator | `SignerBondV2.t.sol`, `ZecuredCompensator.t.sol`; Halmos `check_depositOnlyAddsItsAmountToItsSignersBond` |
| I20 | `pendingTotal` is the sum of the pending activations; each tag is the note of exactly one reservation, activation or Refund claim | Intake, Registry | invariant `I13_I20` |
| I21 | A claim without a destination is never committed or confirmed | `_commitClaim` (`ClaimDestinationUnset`) | invariant `I16_I21`; attacks `attackUnsetDestination`, `attackRefundDestination` |
| I22 | No gate (pause, cap, unbonding, halt) makes an exit or a payout revert | Exits and payouts never read the gates | invariant `I18_I22_I23_I24`; `PendingActivation.t.sol` |
| I23 | The zZEC supply never grows while minting is halted, paused or not fully bonded | `finalizeActivation` checks every gate | invariant `I18_I22_I23_I24`; Halmos `check_K3_onlyActivationMints` |
| I24 | A frozen vault (an open F1 or F2 case) has no exit; a compromised vault takes no new claim, except the refund of a pending activation, which the compensator waits for; a retired vault's outstanding and collateral are zero (and its debt, when repaid in full) | `_activeVault`, `_checkNotCompromised`, `retireVault` | invariant `I18_I22_I23_I24`; `Tier3Slashing.t.sol`, `RegistryV3Hooks.t.sol`, `RedTeamRegressions.t.sol` |
| I25 | Compensation pays its layers in order, each in full before the next, and never more than it received | `CompensationMath` | Halmos `check_takeFillsTheLayersInOrderWithinTheFunds`, `check_aWholeLayerIsPaidInFullAndAnUntouchedOneNothing`; `ZecuredCompensator.t.sol` |
| I26 | The bonds pay a claim at most once; a bond-paid claim's destination is zero or the one three signers gave; never a frozen vault's claim or a Fee claim; the bonds lose exactly what is paid | `slashLiveness`, `markBondPaid`, `setReimburseDestination` | `SignerBondV2.t.sol`, `RegistryV3Hooks.t.sol`, `Disputes.t.sol` |
| I27 | An activated vault owes exactly one Fee claim of `feeBps` of its deposit when that reaches `minFeeZat`, and none otherwise | `activate` | invariant `I27` |
| I28 | Only an active version verifies a proof, and there is always an active violation version | ZecuredVersions, Intake, slasher | `Versions.t.sol`; `DepositProofs.t.sol`, `Tier3Slashing.t.sol`, `Disputes.t.sol` |
| I29 | The relay's best chain is valid under its network's consensus rules | ZcashHeaderRelay with per-network parameters | relay tests on real Testnet and Mainnet headers |
| I30 | (Off-chain) a note that a deployed guest can make a lineage note is never spent except by a committed payout | Signer nodes' policy | node policy tests |
| I31 | A disputed claim counts as unpaid until a proof of its payout upholds it or the inclusion window ends; `lockedZatoshi` is the zZEC supply plus every pending or disputed claim | `IZecuredDisputes` in `ZecuredSpendLogic` | invariant `disputes_andLockedZatoshi`; `Disputes.t.sol` |

The attacks in the campaign (`invariant_attacksAlwaysFail`) must never succeed: reusing a claim
against another note, confirming an uncommitted claim, committing a claim without a destination,
moving a bound refund's destination, landing one note twice, a weak `VaultOpened`, activating
outside the Intake, an account withdrawing its own collateral, liquidating or partially releasing
outside the rules, flash-loaned positions, and replayed or expired close and deleverage requests.
