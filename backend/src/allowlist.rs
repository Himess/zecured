//! The paymaster allowlist (docs/INTERFACES.md § 4.1): which UserOps we sponsor.
//!
//! [`Allowlist::check`] is the pure part. A UserOp passes only if
//! - it is a v0.7 UserOp (no v0.6 `initCode` / `paymasterAndData`) whose gas fields are below
//!   sane caps;
//! - its account deployment, if any, is a Safe v1.4.1 created by the canonical proxy factory
//!   with the app's exact configuration, **at `sender`** (CREATE2);
//! - every call it makes is on the list below, with no ETH value anywhere, and a MultiSend
//!   batch has at most [`MAX_BATCH_CALLS`] calls.
//!
//! It returns the [`Bindings`] the calls need from the chain, which the proxy (proxy.rs) checks
//! along with the rest of what needs the chain: the sender must be a Zecured account, bound to a
//! vault by the deposit proof that reserved it (`Registry.vaultOf(sender) != 0`, before its first
//! UserOp), except for the calls about one of its own claims, which the claim itself binds to the
//! sender (v3: a deposit that landed only as a refund binds no account): a refund's destination,
//! disputing or reopening a payout confirmation, having the bonds pay an overdue claim (only
//! when they can pay it now) and arming a claim's liveness with a signer's receipt (R-5, only when
//! the arming succeeds now); `Intake.refundPending` only for the sender's own vault; the account's
//! own pre-liquidation terms (section 4 of the v4 design: the Settlement Adapter reads them for
//! `msg.sender`, the account itself), within the bounds the Adapter enforces and only when the
//! Adapter would take them now; once deployed, the sender must still be the canonical SafeProxy
//! with the Safe 4337 module as fallback handler; the EntryPoint must be v0.7; senders are
//! rate-limited. The smart account therefore never needs (or receives) ETH, and the paymaster
//! cannot be used for anything but Zecured flows.
//!
//! Refusal reasons are fixed public strings (L-2).

use alloy::primitives::{Address, B256, Bytes, U256, address, b256, hex, keccak256};
use alloy::{sol, sol_types::SolCall};
use serde::Deserialize;

/// Canonical Safe v1.4.1 deployments (same on every chain), as used by permissionless.js.
pub const SAFE_PROXY_FACTORY: Address = address!("4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67");
pub const SAFE_SINGLETON: Address = address!("41675C099F32341bf84BFc5382aF534df5C7461a");
pub const MULTI_SEND_CALL_ONLY: Address = address!("9641d764fc13c8B624c04430C7356C1C7C8102e2");
pub const SAFE_MODULE_SETUP: Address = address!("2dd68b007B46fBe91B9A7c3EDa5A7a1063cB5b47");
pub const SAFE_4337_MODULE: Address = address!("75cf11467937ce3F2f357CE24ffc3DBF8fD5c226");
/// ERC-4337 EntryPoint v0.7, the only one the Safe 4337 module v0.3.0 supports.
pub const ENTRY_POINT_V07: Address = address!("0000000071727De22E5E9d8BAf0edAc6f37da032");

/// `SafeProxyFactory.proxyCreationCode()` of Safe v1.4.1 (identical on Base Sepolia, checked
/// with `cast call`; also app/lib/safe.ts).
pub const SAFE_PROXY_CREATION_CODE: &[u8] = &hex!(
    "608060405234801561001057600080fd5b506040516101e63803806101e68339818101604052602081101561003357600080fd5b8101908080519060200190929190505050600073ffffffffffffffffffffffffffffffffffffffff168173ffffffffffffffffffffffffffffffffffffffff1614156100ca576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004018080602001828103825260228152602001806101c46022913960400191505060405180910390fd5b806000806101000a81548173ffffffffffffffffffffffffffffffffffffffff021916908373ffffffffffffffffffffffffffffffffffffffff1602179055505060ab806101196000396000f3fe608060405273ffffffffffffffffffffffffffffffffffffffff600054167fa619486e0000000000000000000000000000000000000000000000000000000060003514156050578060005260206000f35b3660008037600080366000845af43d6000803e60008114156070573d6000fd5b3d6000f3fea264697066735822122003d1488ee65e08fa41e58e888a9865554c535f2c77126a82cb4c0f917f31441364736f6c63430007060033496e76616c69642073696e676c65746f6e20616464726573732070726f7669646564"
);
/// The deployed SafeProxy code: the 0xab bytes the constructor copies from offset 0x119.
pub const SAFE_PROXY_RUNTIME_CODE: &[u8] = SAFE_PROXY_CREATION_CODE.split_at(0x119).1.split_at(0xab).0;
/// Safe storage slot of the fallback handler: `keccak256("fallback_manager.handler.address")`.
pub const FALLBACK_HANDLER_SLOT: B256 = b256!("6c9a6c4a39284e37ed1cf53d337577d14212a4870fb976a4366c693b939918d5");

/// Most calls in one sponsored MultiSend batch (the app's largest batch has 4).
pub const MAX_BATCH_CALLS: usize = 8;
/// Execution gas caps per UserOp: generous for the app's UserOps (a Safe deployment plus a
/// Morpho borrow), small enough to bound the work one sponsored UserOp can do.
pub const MAX_CALL_GAS: u64 = 3_000_000;
pub const MAX_VERIFICATION_GAS: u64 = 2_000_000;
pub const MAX_PAYMASTER_GAS: u64 = 1_000_000;
/// On Base this carries the L1 data fee divided by the L2 gas price, so it grows when L2 gas is
/// cheap; the cost cap below is what bounds it in money.
pub const MAX_PRE_VERIFICATION_GAS: u64 = 50_000_000;
/// 10 gwei: far above Base's usual fees.
pub const MAX_FEE_PER_GAS: u64 = 10_000_000_000;
/// Most one UserOp may cost the paymaster: all its gas limits at `maxFeePerGas` (0.01 ETH).
pub const MAX_USEROP_COST_WEI: u64 = 10_000_000_000_000_000;

const OPERATION_CALL: u8 = 0;
const OPERATION_DELEGATECALL: u8 = 1;

sol! {
    struct MarketParams {
        address loanToken;
        address collateralToken;
        address oracle;
        address irm;
        uint256 lltv;
    }

    function executeUserOp(address to, uint256 value, bytes data, uint8 operation);
    function executeUserOpWithErrorString(address to, uint256 value, bytes data, uint8 operation);
    function multiSend(bytes transactions);
    function createProxyWithNonce(address singleton, bytes initializer, uint256 saltNonce);
    function setup(address[] owners, uint256 threshold, address to, bytes data, address fallbackHandler, address paymentToken, uint256 payment, address paymentReceiver);
    function enableModules(address[] modules);

    function setAuthorization(address authorized, bool newIsAuthorized);
    function borrow(MarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, address receiver);
    function repay(MarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, bytes data);
    function approve(address spender, uint256 amount);
    function transfer(address to, uint256 amount);
    function requestRelease(bytes32 vaultId, bytes32 destCommitment);
    function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment);
    function setRefundDestination(bytes32 claimId, bytes32 destCommitment);
    function disputePayout(bytes32 claimId);
    function reopenDisputedClaim(bytes32 claimId);
    function invalidateNonce(uint256 nonce);
    function refundPending(bytes32 vaultId);
    function slashLiveness(bytes32 claimId);

    /// v4 (R-5): `SignerBondV2.armLiveness`: a signer's `DestinationReceived` receipt starts the
    /// claim's liveness clock (permissionless on the bond; sponsored for the claim's claimant).
    function armLiveness(bytes32 claimId, uint64 receivedAt, bytes signature);

    /// v4 (pre-liquidation): the Settlement Adapter's terms, in WAD (V4 freeze section 5).
    struct PreLiquidationTerms {
        uint256 preLltv;
        uint256 preLcf1;
        uint256 preLcf2;
        uint256 preLif1;
        uint256 preLif2;
        uint64 expiry;
    }

    function setPreLiquidation(PreLiquidationTerms t);
    function clearPreLiquidation();
}

/// WAD, Morpho's fixed-point one.
const WAD: u128 = 1_000_000_000_000_000_000;
/// Morpho Blue's `LIQUIDATION_CURSOR` and `MAX_LIQUIDATION_INCENTIVE_FACTOR` (ConstantsLib).
const LIQUIDATION_CURSOR: u128 = 300_000_000_000_000_000;
const MAX_LIQUIDATION_INCENTIVE_FACTOR: u128 = 1_150_000_000_000_000_000;

/// Morpho Blue's liquidation incentive factor at `lltv` (WAD): `min(1.15, 1 / (1 - 0.3 (1 - lltv)))`,
/// rounded down as `wDivDown(WAD, WAD - wMulDown(LIQUIDATION_CURSOR, WAD - lltv))`. `None` for an
/// LLTV that is not below one.
pub fn liquidation_incentive_factor(lltv: U256) -> Option<U256> {
    let wad = U256::from(WAD);
    if lltv >= wad {
        return None;
    }
    let cursor_term = U256::from(LIQUIDATION_CURSOR) * (wad - lltv) / wad;
    let lif = wad * wad / (wad - cursor_term);
    Some(lif.min(U256::from(MAX_LIQUIDATION_INCENTIVE_FACTOR)))
}

/// The bounds the Settlement Adapter puts on pre-liquidation terms (V4 freeze section 5, the v4
/// design's section 4): `preLltv < LLTV`, `WAD <= preLif1 <= preLif2 <= LIF(LLTV)`,
/// `0 < preLcf1 <= preLcf2 <= WAD`, and an expiry (checked against the chain's time by the
/// Adapter, and by the proxy's simulation). Terms outside them would only revert.
pub fn check_pre_liquidation_terms(t: &PreLiquidationTerms, lltv: U256) -> Check {
    let wad = U256::from(WAD);
    let lif = liquidation_incentive_factor(lltv).ok_or("pre-liquidation terms out of bounds")?;
    need(
        t.preLltv < lltv
            && wad <= t.preLif1
            && t.preLif1 <= t.preLif2
            && t.preLif2 <= lif
            && !t.preLcf1.is_zero()
            && t.preLcf1 <= t.preLcf2
            && t.preLcf2 <= wad
            && t.expiry != 0,
        "pre-liquidation terms out of bounds",
    )
}

/// Deployment-specific parameters of the allowlist.
#[derive(Clone, Debug, Deserialize)]
pub struct Allowlist {
    pub morpho: Address,
    pub registry: Address,
    pub adapter: Address,
    /// ZecuredIntake (v3): `refundPending`.
    pub intake: Address,
    /// SignerBondV2 (v3), the Registry's bond: `slashLiveness`.
    pub bond: Address,
    pub usdc: Address,
    pub loan_token: Address,
    pub collateral_token: Address,
    pub oracle: Address,
    pub irm: Address,
    pub lltv: U256,
}

/// The fields of a v0.7 UserOp (ERC-7769 JSON) the allowlist looks at.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserOpView {
    pub sender: Address,
    #[serde(default)]
    pub factory: Option<Address>,
    #[serde(default)]
    pub factory_data: Option<Bytes>,
    pub call_data: Bytes,
    /// v0.6 only: never sponsored.
    #[serde(default)]
    pub init_code: Option<Bytes>,
    /// v0.6 only: never sponsored.
    #[serde(default)]
    pub paymaster_and_data: Option<Bytes>,
    #[serde(default)]
    pub call_gas_limit: Option<U256>,
    #[serde(default)]
    pub verification_gas_limit: Option<U256>,
    #[serde(default)]
    pub pre_verification_gas: Option<U256>,
    #[serde(default)]
    pub max_fee_per_gas: Option<U256>,
    #[serde(default)]
    pub max_priority_fee_per_gas: Option<U256>,
    #[serde(default)]
    pub paymaster_verification_gas_limit: Option<U256>,
    #[serde(default)]
    pub paymaster_post_op_gas_limit: Option<U256>,
}

/// A refusal: a fixed public reason.
pub type Check = Result<(), &'static str>;

/// What a sponsorable UserOp's calls need from the chain (proxy.rs checks them before sponsoring).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bindings {
    /// Some call is one that only an account bound to a vault makes (every call but those about
    /// one of the sender's claims): `Registry.vaultOf(sender) != 0`.
    pub bound_sender: bool,
    /// `Intake.refundPending(vaultId)`: each the sender's own vault, `Registry.vaultOf(sender)`.
    pub own_vaults: Vec<B256>,
    /// `Registry.setRefundDestination(claimId, _)`: each a Refund claim of a vault whose account is
    /// the sender (`Registry.claimDetails(claimId)`: kind Refund, claimant the sender).
    pub refund_claims: Vec<B256>,
    /// `Registry.disputePayout(claimId)`, `Registry.reopenDisputedClaim(claimId)` (T-05 (a), served
    /// by the Registry's fallback): each a claim whose claimant is the sender.
    pub own_claims: Vec<B256>,
    /// `SignerBond.slashLiveness(claimId)` (T-05): each a claim whose claimant is the sender, which
    /// the bonds can pay now (`livenessDue` due and non-zero, covered by `totalSlashable`).
    pub liveness_claims: Vec<B256>,
    /// `SignerBond.armLiveness(claimId, receivedAt, signature)` (R-5): each a claim whose claimant
    /// is the sender (the call itself is in `simulated`: the receipt must arm the claim now).
    pub arm_claims: Vec<B256>,
    /// Calls that must succeed now when the sender makes them (an `eth_call` from the sender at the
    /// latest block): an arming (R-5: a signer's signature over the claim's current commitment, not
    /// in the future) and the account's pre-liquidation terms (the Adapter's own checks, its time
    /// included). The paymaster never pays for a call the contract would refuse.
    pub simulated: Vec<(Address, Bytes)>,
}

fn need(ok: bool, reason: &'static str) -> Check {
    if ok { Ok(()) } else { Err(reason) }
}

fn decode<C: SolCall>(data: &[u8]) -> Result<C, &'static str> {
    C::abi_decode(data).map_err(|_| "malformed call data")
}

fn at_most(value: Option<U256>, max: u64) -> bool {
    value.is_none_or(|v| v <= U256::from(max))
}

/// The most `op` can cost: every gas limit it states, at its `maxFeePerGas` (fields still being
/// estimated count as zero). Saturating: absurd values only make it larger.
fn max_cost(op: &UserOpView) -> U256 {
    let gas = [
        op.call_gas_limit,
        op.verification_gas_limit,
        op.pre_verification_gas,
        op.paymaster_verification_gas_limit,
        op.paymaster_post_op_gas_limit,
    ]
    .into_iter()
    .flatten()
    .fold(U256::ZERO, |sum, g| sum.saturating_add(g));
    gas.saturating_mul(op.max_fee_per_gas.unwrap_or_default())
}

/// The address `SafeProxyFactory.createProxyWithNonce(singleton, initializer, saltNonce)`
/// deploys: CREATE2 with salt `keccak256(keccak256(initializer) || saltNonce)` and init code
/// `proxyCreationCode || abi.encode(singleton)`.
pub fn safe_address(singleton: Address, initializer: &[u8], salt_nonce: U256) -> Address {
    let mut salt = keccak256(initializer).to_vec();
    salt.extend_from_slice(&salt_nonce.to_be_bytes::<32>());
    let salt = keccak256(salt);
    let mut init_code = SAFE_PROXY_CREATION_CODE.to_vec();
    init_code.extend_from_slice(&[0u8; 12]);
    init_code.extend_from_slice(singleton.as_slice());
    SAFE_PROXY_FACTORY.create2_from_code(salt, init_code)
}

impl Allowlist {
    fn is_our_market(&self, m: &MarketParams) -> bool {
        m.loanToken == self.loan_token
            && m.collateralToken == self.collateral_token
            && m.oracle == self.oracle
            && m.irm == self.irm
            && m.lltv == self.lltv
    }

    /// Decides whether to sponsor `op` (the parts that need no chain access); returns what its
    /// calls need from the chain.
    pub fn check(&self, op: &UserOpView) -> Result<Bindings, &'static str> {
        need(
            op.init_code.as_ref().is_none_or(|c| c.is_empty())
                && op.paymaster_and_data.as_ref().is_none_or(|p| p.is_empty()),
            "only v0.7 UserOperations are sponsored",
        )?;
        need(
            at_most(op.call_gas_limit, MAX_CALL_GAS)
                && at_most(op.verification_gas_limit, MAX_VERIFICATION_GAS)
                && at_most(op.pre_verification_gas, MAX_PRE_VERIFICATION_GAS)
                && at_most(op.paymaster_verification_gas_limit, MAX_PAYMASTER_GAS)
                && at_most(op.paymaster_post_op_gas_limit, MAX_PAYMASTER_GAS)
                && at_most(op.max_fee_per_gas, MAX_FEE_PER_GAS)
                && at_most(op.max_priority_fee_per_gas, MAX_FEE_PER_GAS)
                && at_most(Some(max_cost(op)), MAX_USEROP_COST_WEI),
            "gas limits or fees above the sponsorship caps",
        )?;
        match op.factory {
            Some(factory) => self.check_deployment(op, factory)?,
            None => need(op.factory_data.as_ref().is_none_or(|d| d.is_empty()), "factory data without a factory")?,
        }
        let data = op.call_data.as_ref();
        let (to, value, inner, operation) = if let Ok(c) = executeUserOpCall::abi_decode(data) {
            (c.to, c.value, c.data, c.operation)
        } else if let Ok(c) = executeUserOpWithErrorStringCall::abi_decode(data) {
            (c.to, c.value, c.data, c.operation)
        } else {
            return Err("callData is not a Safe 4337 execution");
        };
        need(value.is_zero(), "no ETH value allowed")?;
        let mut bindings = Bindings::default();
        match operation {
            OPERATION_CALL => self.check_call(op.sender, to, &inner, &mut bindings)?,
            OPERATION_DELEGATECALL if to == MULTI_SEND_CALL_ONLY => {
                let batch = decode::<multiSendCall>(&inner)?;
                let calls = unpack_multisend(&batch.transactions)?;
                need(!calls.is_empty() && calls.len() <= MAX_BATCH_CALLS, "batch is empty or too long")?;
                for (op_kind, target, value, data) in calls {
                    need(op_kind == OPERATION_CALL, "batched delegatecall not allowed")?;
                    need(value.is_zero(), "no ETH value allowed")?;
                    self.check_call(op.sender, target, &data, &mut bindings)?;
                }
            }
            _ => return Err("operation not allowed"),
        }
        Ok(bindings)
    }

    /// The exact account the app creates: one owner, threshold 1, the 4337 module enabled
    /// through SafeModuleSetup and set as fallback handler, no payment; deployed at `sender`.
    fn check_deployment(&self, op: &UserOpView, factory: Address) -> Check {
        need(factory == SAFE_PROXY_FACTORY, "unknown account factory")?;
        let data = op.factory_data.as_ref().map(|d| d.as_ref()).unwrap_or_default();
        let call = decode::<createProxyWithNonceCall>(data)?;
        need(call.singleton == SAFE_SINGLETON, "unexpected Safe singleton")?;
        let s = decode::<setupCall>(&call.initializer)?;
        need(s.owners.len() == 1 && s.threshold == U256::from(1u8), "unexpected Safe owners")?;
        need(s.to == SAFE_MODULE_SETUP, "unexpected Safe setup target")?;
        let modules = decode::<enableModulesCall>(&s.data)?;
        need(modules.modules == [SAFE_4337_MODULE], "unexpected Safe modules")?;
        need(s.fallbackHandler == SAFE_4337_MODULE, "unexpected fallback handler")?;
        need(
            s.paymentToken == Address::ZERO && s.payment.is_zero() && s.paymentReceiver == Address::ZERO,
            "Safe setup payment not allowed",
        )?;
        need(
            safe_address(call.singleton, &call.initializer, call.saltNonce) == op.sender,
            "the deployment does not create the sender",
        )
    }

    fn check_call(&self, sender: Address, to: Address, data: &[u8], bindings: &mut Bindings) -> Check {
        need(data.len() >= 4, "missing selector")?;
        let selector: [u8; 4] = [data[0], data[1], data[2], data[3]];
        // The calls about one of the sender's own claims, which an account not bound to a vault may
        // make too (a deposit that landed only as a refund binds none); the proxy checks the claim
        // is the sender's. T-02: a refund's destination.
        if to == self.registry && selector == setRefundDestinationCall::SELECTOR {
            let c = decode::<setRefundDestinationCall>(data)?;
            need(!c.destCommitment.is_zero(), "a refund destination cannot be zero")?;
            bindings.refund_claims.push(c.claimId);
            return Ok(());
        }
        // T-05 (a): disputing a payout confirmation, and reopening the claim once nobody proved it.
        if to == self.registry && selector == disputePayoutCall::SELECTOR {
            bindings.own_claims.push(decode::<disputePayoutCall>(data)?.claimId);
            return Ok(());
        }
        if to == self.registry && selector == reopenDisputedClaimCall::SELECTOR {
            bindings.own_claims.push(decode::<reopenDisputedClaimCall>(data)?.claimId);
            return Ok(());
        }
        // T-05: the bonds pay an overdue claim to its claimant (that one selector on the bond); the
        // proxy checks the claim is the sender's and that the bonds can pay it now.
        if to == self.bond && !self.bond.is_zero() && selector == slashLivenessCall::SELECTOR {
            let claim_id = decode::<slashLivenessCall>(data)?.claimId;
            // A second payment of the same claim would revert: never sponsor a doomed call.
            need(!bindings.liveness_claims.contains(&claim_id), "a claim is paid from the bonds only once")?;
            bindings.liveness_claims.push(claim_id);
            return Ok(());
        }
        // R-5: a signer's receipt arms the claim's liveness; the proxy checks the claim is the
        // sender's and that the bond takes the receipt now (simulated).
        if to == self.bond && !self.bond.is_zero() && selector == armLivenessCall::SELECTOR {
            let claim_id = decode::<armLivenessCall>(data)?.claimId;
            need(!bindings.arm_claims.contains(&claim_id), "a claim is armed once per operation")?;
            bindings.arm_claims.push(claim_id);
            bindings.simulated.push((to, Bytes::copy_from_slice(data)));
            return Ok(());
        }
        bindings.bound_sender = true;
        // v4 pre-liquidation: the Adapter keeps terms for `msg.sender`, so these calls only ever set
        // or clear the sender's own; the terms must be within the Adapter's bounds, and the proxy
        // checks the Adapter takes them now (its expiry against the chain's time, among others).
        if to == self.adapter && selector == setPreLiquidationCall::SELECTOR {
            let c = decode::<setPreLiquidationCall>(data)?;
            check_pre_liquidation_terms(&c.t, self.lltv)?;
            bindings.simulated.push((to, Bytes::copy_from_slice(data)));
            return Ok(());
        }
        if to == self.adapter && selector == clearPreLiquidationCall::SELECTOR {
            decode::<clearPreLiquidationCall>(data)?;
            bindings.simulated.push((to, Bytes::copy_from_slice(data)));
            return Ok(());
        }
        // T-03: the proxy checks it is the sender's own vault.
        if to == self.intake && selector == refundPendingCall::SELECTOR {
            bindings.own_vaults.push(decode::<refundPendingCall>(data)?.vaultId);
            return Ok(());
        }
        if to == self.morpho {
            if selector == setAuthorizationCall::SELECTOR {
                let c = decode::<setAuthorizationCall>(data)?;
                return need(
                    c.authorized == self.registry || c.authorized == self.adapter,
                    "authorization only for the registry or the adapter",
                );
            }
            if selector == borrowCall::SELECTOR {
                let c = decode::<borrowCall>(data)?;
                need(self.is_our_market(&c.marketParams), "not the Zecured market")?;
                return need(c.onBehalf == sender, "borrow must be for the account itself");
            }
            if selector == repayCall::SELECTOR {
                let c = decode::<repayCall>(data)?;
                need(self.is_our_market(&c.marketParams), "not the Zecured market")?;
                need(c.onBehalf == sender, "repay must be for the account itself")?;
                return need(c.data.is_empty(), "repay callbacks not allowed");
            }
        } else if to == self.usdc {
            if selector == approveCall::SELECTOR {
                return need(decode::<approveCall>(data)?.spender == self.morpho, "approve only to Morpho");
            }
            if selector == transferCall::SELECTOR {
                decode::<transferCall>(data)?;
                return Ok(());
            }
        } else if to == self.registry && selector == requestReleaseCall::SELECTOR {
            decode::<requestReleaseCall>(data)?;
            return Ok(());
        } else if to == self.registry && selector == requestPartialReleaseCall::SELECTOR {
            decode::<requestPartialReleaseCall>(data)?;
            return Ok(());
        } else if to == self.adapter && selector == invalidateNonceCall::SELECTOR {
            decode::<invalidateNonceCall>(data)?;
            return Ok(());
        }
        Err("call not on the allowlist")
    }
}

/// One MultiSend entry: operation, target, value, calldata.
type MultiSendEntry = (u8, Address, U256, Vec<u8>);

/// MultiSend packing: operation (1) | to (20) | value (32) | data length (32) | data.
fn unpack_multisend(mut packed: &[u8]) -> Result<Vec<MultiSendEntry>, &'static str> {
    const HEADER: usize = 1 + 20 + 32 + 32;
    let mut out = vec![];
    while !packed.is_empty() {
        need(out.len() < MAX_BATCH_CALLS, "batch is empty or too long")?;
        need(packed.len() >= HEADER, "truncated multiSend entry")?;
        let op = packed[0];
        let to = Address::from_slice(&packed[1..21]);
        let value = U256::from_be_slice(&packed[21..53]);
        let len = usize::try_from(U256::from_be_slice(&packed[53..85])).map_err(|_| "truncated multiSend data")?;
        // No `HEADER + len`: an attacker-chosen length must not overflow.
        need(packed.len() - HEADER >= len, "truncated multiSend data")?;
        out.push((op, to, value, packed[HEADER..HEADER + len].to_vec()));
        packed = &packed[HEADER + len..];
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloy::primitives::B256;

    pub fn list() -> Allowlist {
        Allowlist {
            morpho: Address::repeat_byte(0x11),
            registry: Address::repeat_byte(0x22),
            adapter: Address::repeat_byte(0x33),
            intake: Address::repeat_byte(0x88),
            bond: Address::repeat_byte(0xb0),
            usdc: Address::repeat_byte(0x44),
            loan_token: Address::repeat_byte(0x44),
            collateral_token: Address::repeat_byte(0x55),
            oracle: Address::repeat_byte(0x66),
            irm: Address::repeat_byte(0x77),
            lltv: U256::from(625_000_000_000_000_000u64),
        }
    }

    fn market(l: &Allowlist) -> MarketParams {
        MarketParams {
            loanToken: l.loan_token,
            collateralToken: l.collateral_token,
            oracle: l.oracle,
            irm: l.irm,
            lltv: l.lltv,
        }
    }

    pub fn single(to: Address, data: Vec<u8>) -> Bytes {
        executeUserOpCall {
            to,
            value: U256::ZERO,
            data: data.into(),
            operation: OPERATION_CALL,
        }
        .abi_encode()
        .into()
    }

    fn pack(calls: &[(Address, Vec<u8>)]) -> Vec<u8> {
        let mut packed = vec![];
        for (to, data) in calls {
            packed.push(OPERATION_CALL);
            packed.extend_from_slice(to.as_slice());
            packed.extend_from_slice(&[0u8; 32]);
            packed.extend_from_slice(&U256::from(data.len()).to_be_bytes::<32>());
            packed.extend_from_slice(data);
        }
        packed
    }

    fn multisend(packed: Vec<u8>) -> Bytes {
        executeUserOpCall {
            to: MULTI_SEND_CALL_ONLY,
            value: U256::ZERO,
            data: multiSendCall {
                transactions: packed.into(),
            }
            .abi_encode()
            .into(),
            operation: OPERATION_DELEGATECALL,
        }
        .abi_encode()
        .into()
    }

    pub fn batch(calls: &[(Address, Vec<u8>)]) -> Bytes {
        multisend(pack(calls))
    }

    pub fn op(sender: Address, call_data: Bytes) -> UserOpView {
        UserOpView {
            sender,
            call_data,
            ..Default::default()
        }
    }

    fn set_auth(l: &Allowlist, who: Address) -> (Address, Vec<u8>) {
        (
            l.morpho,
            setAuthorizationCall {
                authorized: who,
                newIsAuthorized: true,
            }
            .abi_encode(),
        )
    }

    fn release() -> Vec<u8> {
        requestReleaseCall {
            vaultId: B256::repeat_byte(1),
            destCommitment: B256::repeat_byte(2),
        }
        .abi_encode()
    }

    pub fn refund_destination(claim: u8) -> Vec<u8> {
        setRefundDestinationCall {
            claimId: B256::repeat_byte(claim),
            destCommitment: B256::repeat_byte(2),
        }
        .abi_encode()
    }

    pub fn refund_pending(vault: u8) -> Vec<u8> {
        refundPendingCall {
            vaultId: B256::repeat_byte(vault),
        }
        .abi_encode()
    }

    /// T-02, T-03: a refund's destination and the refund of a long-paused activation are
    /// sponsored, each with the binding the proxy then checks on the chain.
    #[test]
    fn refunds_are_sponsored_with_their_bindings() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        // Destinations bind no vault: a deposit that landed only as a refund binds no account.
        let calls = [(l.registry, refund_destination(3)), (l.registry, refund_destination(4))];
        assert_eq!(
            l.check(&op(me, batch(&calls))),
            Ok(Bindings {
                bound_sender: false,
                own_vaults: vec![],
                refund_claims: vec![B256::repeat_byte(3), B256::repeat_byte(4)],
                own_claims: vec![],
                liveness_claims: vec![],
                ..Default::default()
            })
        );
        // Such an account's first UserOp deploys it.
        let owner = Address::repeat_byte(0x01);
        let mut first = op(predicted(owner, U256::ZERO), single(l.registry, refund_destination(3)));
        (first.factory, first.factory_data) = deployment(owner, SAFE_4337_MODULE, U256::ZERO);
        assert!(!l.check(&first).unwrap().bound_sender);
        // refundPending, on the Intake: the sender's own vault.
        assert_eq!(
            l.check(&op(me, single(l.intake, refund_pending(7)))),
            Ok(Bindings {
                bound_sender: true,
                own_vaults: vec![B256::repeat_byte(7)],
                refund_claims: vec![],
                own_claims: vec![],
                liveness_claims: vec![],
                ..Default::default()
            })
        );
        // Every other call needs an account bound to a vault, also next to a destination.
        let mixed = [(l.registry, refund_destination(3)), (l.registry, release())];
        let b = l.check(&op(me, batch(&mixed))).unwrap();
        assert!(b.bound_sender);
        assert_eq!(b.refund_claims, vec![B256::repeat_byte(3)]);
        assert!(l.check(&op(me, single(l.registry, release()))).unwrap().bound_sender);
    }

    #[test]
    fn refund_calls_go_only_to_their_own_contract() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        assert_eq!(l.check(&op(me, single(l.intake, refund_destination(3)))), Err("call not on the allowlist"));
        assert_eq!(l.check(&op(me, single(l.registry, refund_pending(7)))), Err("call not on the allowlist"));
        assert_eq!(l.check(&op(me, single(l.adapter, refund_pending(7)))), Err("call not on the allowlist"));
        let zero = setRefundDestinationCall {
            claimId: B256::repeat_byte(3),
            destCommitment: B256::ZERO,
        }
        .abi_encode();
        assert_eq!(l.check(&op(me, single(l.registry, zero))), Err("a refund destination cannot be zero"));
    }

    pub fn dispute(claim: u8) -> Vec<u8> {
        disputePayoutCall {
            claimId: B256::repeat_byte(claim),
        }
        .abi_encode()
    }

    pub fn reopen(claim: u8) -> Vec<u8> {
        reopenDisputedClaimCall {
            claimId: B256::repeat_byte(claim),
        }
        .abi_encode()
    }

    /// T-05 (a): disputing a payout confirmation and reopening the disputed claim, on the
    /// Registry's address (its fallback serves them), each for a claim of the sender's own.
    #[test]
    fn disputes_are_sponsored_for_the_senders_own_claims() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        assert_eq!(
            l.check(&op(me, batch(&[(l.registry, dispute(5)), (l.registry, reopen(6))]))),
            Ok(Bindings {
                bound_sender: false,
                own_vaults: vec![],
                refund_claims: vec![],
                own_claims: vec![B256::repeat_byte(5), B256::repeat_byte(6)],
                liveness_claims: vec![],
                ..Default::default()
            })
        );
        // A claim binds its account: no vault needed, unless another call needs one.
        let mixed = [(l.registry, dispute(5)), (l.registry, release())];
        assert!(l.check(&op(me, batch(&mixed))).unwrap().bound_sender);
        for to in [l.intake, l.adapter, l.morpho] {
            assert_eq!(l.check(&op(me, single(to, dispute(5)))), Err("call not on the allowlist"));
            assert_eq!(l.check(&op(me, single(to, reopen(5)))), Err("call not on the allowlist"));
        }
    }

    pub fn liveness(claim: u8) -> Vec<u8> {
        slashLivenessCall {
            claimId: B256::repeat_byte(claim),
        }
        .abi_encode()
    }

    /// T-05: the bonds paying an overdue claim, on the bond only, each claim once; the claim binds
    /// the account (the proxy checks its claimant and that the bonds can pay it now).
    #[test]
    fn bond_payments_are_sponsored_on_the_bond_only() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        assert_eq!(
            l.check(&op(me, batch(&[(l.bond, liveness(5)), (l.bond, liveness(6))]))),
            Ok(Bindings {
                bound_sender: false,
                own_vaults: vec![],
                refund_claims: vec![],
                own_claims: vec![],
                liveness_claims: vec![B256::repeat_byte(5), B256::repeat_byte(6)],
                ..Default::default()
            })
        );
        assert_eq!(
            l.check(&op(me, batch(&[(l.bond, liveness(5)), (l.bond, liveness(5))]))),
            Err("a claim is paid from the bonds only once")
        );
        let mixed = [(l.bond, liveness(5)), (l.registry, release())];
        assert!(l.check(&op(me, batch(&mixed))).unwrap().bound_sender);
        for to in [l.registry, l.intake, l.adapter, l.morpho, l.usdc, Address::ZERO] {
            assert_eq!(l.check(&op(me, single(to, liveness(5)))), Err("call not on the allowlist"));
        }
        // A deployment without a bond sponsors no bond payment, not even to the zero address.
        let no_bond = Allowlist {
            bond: Address::ZERO,
            ..list()
        };
        assert_eq!(no_bond.check(&op(me, single(Address::ZERO, liveness(5)))), Err("call not on the allowlist"));
    }

    /// An arming with a placeholder receipt (the proxy's simulation decides whether it holds).
    pub fn arm(claim: u8) -> Vec<u8> {
        armLivenessCall {
            claimId: B256::repeat_byte(claim),
            receivedAt: 1_800_000_000,
            signature: vec![0x1b; 65].into(),
        }
        .abi_encode()
    }

    const E15: u64 = 1_000_000_000_000_000;

    /// The app's "Standard" preset: from 55 % LTV, 2 % to 5 % incentive, 10 % to 50 % of the debt.
    pub fn standard_terms() -> PreLiquidationTerms {
        PreLiquidationTerms {
            preLltv: U256::from(550 * E15),
            preLcf1: U256::from(100 * E15),
            preLcf2: U256::from(500 * E15),
            preLif1: U256::from(1_020 * E15),
            preLif2: U256::from(1_050 * E15),
            expiry: 1_900_000_000,
        }
    }

    pub fn set_pre_liquidation(t: PreLiquidationTerms) -> Vec<u8> {
        setPreLiquidationCall { t }.abi_encode()
    }

    pub fn clear_pre_liquidation() -> Vec<u8> {
        clearPreLiquidationCall {}.abi_encode()
    }

    /// The SettlementAdapter's selectors (contracts/abi/SettlementAdapter.json, track L-PRE).
    #[test]
    fn pre_liquidation_selectors_match_the_adapter() {
        assert_eq!(setPreLiquidationCall::SELECTOR, [0x3b, 0x8f, 0x1f, 0xc8]);
        assert_eq!(clearPreLiquidationCall::SELECTOR, [0xad, 0x90, 0xc3, 0xb3]);
    }

    /// R-5: arming a claim's liveness with a receipt, on the bond only, each claim once per
    /// operation; the claim binds the account (the proxy checks its claimant), and every arming is
    /// simulated before it is sponsored.
    #[test]
    fn armings_are_sponsored_on_the_bond_and_simulated() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let b = l.check(&op(me, batch(&[(l.bond, arm(5)), (l.bond, arm(6))]))).unwrap();
        assert!(!b.bound_sender, "the claim binds the account");
        assert_eq!(b.arm_claims, vec![B256::repeat_byte(5), B256::repeat_byte(6)]);
        assert_eq!(b.simulated, vec![(l.bond, Bytes::from(arm(5))), (l.bond, Bytes::from(arm(6)))]);
        assert!(b.own_claims.is_empty() && b.liveness_claims.is_empty());
        assert_eq!(
            l.check(&op(me, batch(&[(l.bond, arm(5)), (l.bond, arm(5))]))),
            Err("a claim is armed once per operation")
        );
        // Arming then having the bonds pay in one operation is two checks of one claim.
        let both = l.check(&op(me, batch(&[(l.bond, arm(5)), (l.bond, liveness(5))]))).unwrap();
        assert_eq!((both.arm_claims.len(), both.liveness_claims.len()), (1, 1));
        for to in [l.registry, l.intake, l.adapter, l.morpho, l.usdc, Address::ZERO] {
            assert_eq!(l.check(&op(me, single(to, arm(5)))), Err("call not on the allowlist"));
        }
        let no_bond = Allowlist {
            bond: Address::ZERO,
            ..list()
        };
        assert_eq!(no_bond.check(&op(me, single(Address::ZERO, arm(5)))), Err("call not on the allowlist"));
        assert_eq!(l.check(&op(me, single(l.bond, vec![0x01, 0x02]))), Err("missing selector"));
    }

    /// Morpho's incentive factor, as ConstantsLib and MathLib compute it.
    #[test]
    fn the_liquidation_incentive_factor_is_morphos() {
        let lif = |lltv: u64| liquidation_incentive_factor(U256::from(lltv));
        // 1 / (1 - 0.3 * 0.375) = 1 / 0.8875, rounded down: Zecured's market (12.68 %).
        assert_eq!(lif(625 * E15), Some(U256::from(1_126_760_563_380_281_690u64)));
        // 1 / (1 - 0.3 * 0.14) = 1 / 0.958.
        assert_eq!(lif(860 * E15), Some(U256::from(1_043_841_336_116_910_229u64)));
        assert_eq!(lif(0), Some(U256::from(1_150 * E15)), "capped at 1.15");
        assert_eq!(liquidation_incentive_factor(U256::from(WAD)), None);
    }

    /// v4 pre-liquidation: the account's own terms (`msg.sender` on the Adapter), only from an
    /// account bound to a vault, within the Adapter's bounds, always simulated.
    #[test]
    fn pre_liquidation_terms_are_sponsored_within_the_adapters_bounds() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let set = set_pre_liquidation(standard_terms());
        let b = l.check(&op(me, single(l.adapter, set.clone()))).unwrap();
        assert!(b.bound_sender, "only a loan account bound to a vault");
        assert_eq!(b.simulated, vec![(l.adapter, Bytes::from(set.clone()))]);
        let b = l.check(&op(me, single(l.adapter, clear_pre_liquidation()))).unwrap();
        assert!(b.bound_sender);
        assert_eq!(b.simulated, vec![(l.adapter, Bytes::from(clear_pre_liquidation()))]);
        // Next to a borrow, in the first operation of the account.
        let calls = [(l.adapter, set.clone()), (l.adapter, clear_pre_liquidation())];
        assert_eq!(l.check(&op(me, batch(&calls))).unwrap().simulated.len(), 2);
        // The bounds' edges are inside: the incentive up to the market's own, the whole debt.
        let lif = liquidation_incentive_factor(l.lltv).unwrap();
        let edge = PreLiquidationTerms {
            preLif2: lif,
            preLcf2: U256::from(WAD),
            preLltv: l.lltv - U256::from(1u8),
            ..standard_terms()
        };
        l.check(&op(me, single(l.adapter, set_pre_liquidation(edge)))).unwrap();
        let outside: [fn(&mut PreLiquidationTerms); 8] = [
            |t| t.preLltv = U256::from(625 * E15),
            |t| t.preLif1 = U256::from(WAD - 1),
            |t| t.preLif1 = t.preLif2 + U256::from(1u8),
            |t| t.preLif2 = liquidation_incentive_factor(U256::from(625 * E15)).unwrap() + U256::from(1u8),
            |t| t.preLcf1 = U256::ZERO,
            |t| t.preLcf1 = t.preLcf2 + U256::from(1u8),
            |t| t.preLcf2 = U256::from(WAD + 1),
            |t| t.expiry = 0,
        ];
        for f in outside {
            let mut t = standard_terms();
            f(&mut t);
            assert_eq!(
                l.check(&op(me, single(l.adapter, set_pre_liquidation(t)))),
                Err("pre-liquidation terms out of bounds")
            );
        }
        for to in [l.registry, l.intake, l.bond, l.morpho, l.usdc] {
            assert_eq!(l.check(&op(me, single(to, set.clone()))), Err("call not on the allowlist"));
            assert_eq!(l.check(&op(me, single(to, clear_pre_liquidation()))), Err("call not on the allowlist"));
        }
    }

    sol! {
        function preLiquidate(address account, uint256 repaidShares, bytes32 mmCommitment, uint64 minSeizedZat);
        function upholdPayout(bytes32 claimId, bytes32 payoutRef);
        function finalizeActivation(bytes32 vaultId);
        function dropPendingActivation(bytes32 vaultId);
        function attestDepositWithProof(bytes journal, bytes seal, bytes[] openedSigs);
        function pauseMinting(uint64 until, uint256 nonce, uint64 deadline, bytes[] sigs);
        function abandonRefund(bytes32 claimId);
        function writeOffDust(bytes32 claimId);
        function claimBounty();
        function requestUnbond(uint256 amount);
        function withdrawEquity(bytes32 vaultId);
    }

    /// Nothing else of v3 is sponsored: the Intake's other entry points, the Registry's other
    /// refund and payout calls (anyone's, never the account's), the bond's other calls and the
    /// compensator.
    #[test]
    fn other_v3_calls_are_refused() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let id = B256::repeat_byte(7);
        let compensator = Address::repeat_byte(0xc0);
        let refused: [(Address, Vec<u8>); 11] = [
            // v4: pre-liquidating someone else is the caller's own business, never sponsored.
            (
                l.adapter,
                preLiquidateCall {
                    account: Address::repeat_byte(0xab),
                    repaidShares: U256::from(1u8),
                    mmCommitment: id,
                    minSeizedZat: 0,
                }
                .abi_encode(),
            ),
            // The slasher's call, after an Included proof.
            (
                l.registry,
                upholdPayoutCall {
                    claimId: id,
                    payoutRef: id,
                }
                .abi_encode(),
            ),
            (l.intake, finalizeActivationCall { vaultId: id }.abi_encode()),
            (l.intake, dropPendingActivationCall { vaultId: id }.abi_encode()),
            (
                l.intake,
                attestDepositWithProofCall {
                    journal: Bytes::new(),
                    seal: Bytes::new(),
                    openedSigs: vec![],
                }
                .abi_encode(),
            ),
            (
                l.intake,
                pauseMintingCall {
                    until: 1,
                    nonce: U256::ZERO,
                    deadline: 1,
                    sigs: vec![],
                }
                .abi_encode(),
            ),
            (l.registry, abandonRefundCall { claimId: id }.abi_encode()),
            (l.registry, writeOffDustCall { claimId: id }.abi_encode()),
            (l.bond, claimBountyCall {}.abi_encode()),
            (
                l.bond,
                requestUnbondCall {
                    amount: U256::from(1u8),
                }
                .abi_encode(),
            ),
            (compensator, withdrawEquityCall { vaultId: id }.abi_encode()),
        ];
        for (to, data) in refused {
            assert_eq!(l.check(&op(me, single(to, data))), Err("call not on the allowlist"));
        }
    }

    /// The app's first UserOp: deploy the Safe, authorize Registry and Adapter, borrow.
    #[test]
    fn first_borrow_batch_is_sponsored() {
        let l = list();
        let owner = Address::repeat_byte(0x01);
        let (factory, factory_data) = deployment(owner, SAFE_4337_MODULE, U256::ZERO);
        let me = predicted(owner, U256::ZERO);
        let calls = vec![
            set_auth(&l, l.registry),
            set_auth(&l, l.adapter),
            (
                l.morpho,
                borrowCall {
                    marketParams: market(&l),
                    assets: U256::from(1_000_000u64),
                    shares: U256::ZERO,
                    onBehalf: me,
                    receiver: me,
                }
                .abi_encode(),
            ),
        ];
        let mut op = op(me, batch(&calls));
        (op.factory, op.factory_data) = (factory, factory_data);
        op.call_gas_limit = Some(U256::from(400_000u64));
        op.verification_gas_limit = Some(U256::from(600_000u64));
        op.pre_verification_gas = Some(U256::from(120_000u64));
        op.max_fee_per_gas = Some(U256::from(1_200_000u64));
        l.check(&op).unwrap();
    }

    #[test]
    fn repay_and_release_batch_is_sponsored() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let calls = vec![
            (
                l.usdc,
                approveCall {
                    spender: l.morpho,
                    amount: U256::MAX,
                }
                .abi_encode(),
            ),
            (
                l.morpho,
                repayCall {
                    marketParams: market(&l),
                    assets: U256::ZERO,
                    shares: U256::from(5u8),
                    onBehalf: me,
                    data: Bytes::new(),
                }
                .abi_encode(),
            ),
            set_auth(&l, l.registry),
            (l.registry, release()),
        ];
        l.check(&op(me, batch(&calls))).unwrap();
    }

    fn deployment(owner: Address, module: Address, salt_nonce: U256) -> (Option<Address>, Option<Bytes>) {
        let data = createProxyWithNonceCall {
            singleton: SAFE_SINGLETON,
            initializer: initializer(owner, module).into(),
            saltNonce: salt_nonce,
        }
        .abi_encode();
        (Some(SAFE_PROXY_FACTORY), Some(data.into()))
    }

    fn initializer(owner: Address, module: Address) -> Vec<u8> {
        setupCall {
            owners: vec![owner],
            threshold: U256::from(1u8),
            to: SAFE_MODULE_SETUP,
            data: enableModulesCall { modules: vec![module] }.abi_encode().into(),
            fallbackHandler: module,
            paymentToken: Address::ZERO,
            payment: U256::ZERO,
            paymentReceiver: Address::ZERO,
        }
        .abi_encode()
    }

    fn predicted(owner: Address, salt_nonce: U256) -> Address {
        safe_address(SAFE_SINGLETON, &initializer(owner, SAFE_4337_MODULE), salt_nonce)
    }

    #[test]
    fn only_the_apps_safe_configuration_is_deployed() {
        let l = list();
        let owner = Address::repeat_byte(0x01);
        let me = predicted(owner, U256::ZERO);
        let call = single(
            l.registry,
            requestPartialReleaseCall {
                vaultId: B256::repeat_byte(1),
                amount: 200_000,
                destCommitment: B256::repeat_byte(2),
            }
            .abi_encode(),
        );
        let mut op = op(me, call);
        (op.factory, op.factory_data) = deployment(owner, SAFE_4337_MODULE, U256::ZERO);
        l.check(&op).unwrap();
        (op.factory, op.factory_data) = deployment(owner, Address::repeat_byte(0x02), U256::ZERO);
        assert_eq!(l.check(&op), Err("unexpected Safe modules"));
    }

    /// M-3: the factory data must create exactly `sender`.
    #[test]
    fn the_deployment_must_create_the_sender() {
        let l = list();
        let owner = Address::repeat_byte(0x01);
        let mut op = op(Address::repeat_byte(0x99), single(l.registry, release()));
        (op.factory, op.factory_data) = deployment(owner, SAFE_4337_MODULE, U256::ZERO);
        assert_eq!(l.check(&op), Err("the deployment does not create the sender"));
        op.sender = predicted(owner, U256::ZERO);
        l.check(&op).unwrap();
        // Another salt nonce is another account.
        (op.factory, op.factory_data) = deployment(owner, SAFE_4337_MODULE, U256::from(7u8));
        assert!(l.check(&op).is_err());
        op.factory_data = None;
        assert!(l.check(&op).is_err());
        op.factory = None;
        op.factory_data = Some(Bytes::from(vec![1u8]));
        assert_eq!(l.check(&op), Err("factory data without a factory"));
    }

    /// The CREATE2 derivation against addresses returned by the real SafeProxyFactory
    /// (`eth_call` of createProxyWithNonce on a Base Sepolia fork), owner 0xf39F…2266.
    #[test]
    fn safe_addresses_match_the_factory() {
        let owner: Address = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".parse().unwrap();
        let a: Address = "0x08eA90D108291a05830B25E8eFaF027979dEb8D1".parse().unwrap();
        let b: Address = "0x8a5D1a934DB9a8940A042f3e378873650e6Ea96A".parse().unwrap();
        assert_eq!(predicted(owner, U256::ZERO), a);
        assert_eq!(predicted(owner, U256::from(7u8)), b);
    }

    #[test]
    fn safe_constants() {
        assert_eq!(SAFE_PROXY_CREATION_CODE.len(), 0x1e6);
        // Code of a deployed Safe v1.4.1 proxy (`cast code` on Base Sepolia).
        assert_eq!(
            hex::encode(SAFE_PROXY_RUNTIME_CODE),
            "608060405273ffffffffffffffffffffffffffffffffffffffff600054167fa619486e0000000000000000000000000000000000000000000000000000000060003514156050578060005260206000f35b3660008037600080366000845af43d6000803e60008114156070573d6000fd5b3d6000f3fea264697066735822122003d1488ee65e08fa41e58e888a9865554c535f2c77126a82cb4c0f917f31441364736f6c63430007060033"
        );
        assert_eq!(FALLBACK_HANDLER_SLOT, keccak256("fallback_manager.handler.address"));
    }

    #[test]
    fn everything_else_is_refused() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let stranger = Address::repeat_byte(0xab);
        // authorizing anyone else
        let (to, data) = set_auth(&l, stranger);
        assert!(l.check(&op(me, single(to, data))).is_err());
        // borrowing on behalf of someone else
        let b = borrowCall {
            marketParams: market(&l),
            assets: U256::from(1u8),
            shares: U256::ZERO,
            onBehalf: stranger,
            receiver: me,
        };
        assert!(l.check(&op(me, single(l.morpho, b.abi_encode()))).is_err());
        // another market
        let mut m = market(&l);
        m.lltv = U256::from(1u8);
        let b = borrowCall {
            marketParams: m,
            assets: U256::from(1u8),
            shares: U256::ZERO,
            onBehalf: me,
            receiver: me,
        };
        assert!(l.check(&op(me, single(l.morpho, b.abi_encode()))).is_err());
        // approving a non-Morpho spender
        let a = approveCall {
            spender: stranger,
            amount: U256::MAX,
        };
        assert!(l.check(&op(me, single(l.usdc, a.abi_encode()))).is_err());
        // arbitrary contract call
        assert!(l.check(&op(me, single(stranger, vec![1, 2, 3, 4]))).is_err());
        // ETH value
        let with_value = executeUserOpCall {
            to: l.usdc,
            value: U256::from(1u8),
            data: transferCall {
                to: stranger,
                amount: U256::from(1u8),
            }
            .abi_encode()
            .into(),
            operation: OPERATION_CALL,
        };
        assert!(l.check(&op(me, with_value.abi_encode().into())).is_err());
        // delegatecall to something other than MultiSendCallOnly
        let dc = executeUserOpCall {
            to: stranger,
            value: U256::ZERO,
            data: Bytes::new(),
            operation: OPERATION_DELEGATECALL,
        };
        assert!(l.check(&op(me, dc.abi_encode().into())).is_err());
        // unknown factory
        let mut deploy = op(
            me,
            single(
                l.usdc,
                transferCall {
                    to: stranger,
                    amount: U256::from(1u8),
                }
                .abi_encode(),
            ),
        );
        deploy.factory = Some(stranger);
        assert_eq!(l.check(&deploy), Err("unknown account factory"));
    }

    /// M-3: v0.6 UserOps (whose `initCode` this check would not read) are never sponsored.
    #[test]
    fn v06_userops_are_refused() {
        let l = list();
        let mut op = op(Address::repeat_byte(0x99), single(l.registry, release()));
        l.check(&op).unwrap();
        op.init_code = Some(Bytes::from(vec![0xde, 0xad]));
        assert_eq!(l.check(&op), Err("only v0.7 UserOperations are sponsored"));
        op.init_code = Some(Bytes::new());
        l.check(&op).unwrap();
        op.paymaster_and_data = Some(Bytes::from(vec![1u8; 20]));
        assert!(l.check(&op).is_err());
    }

    /// M-3: gas fields above the caps are refused (absent fields, e.g. while estimating, are fine).
    #[test]
    fn gas_fields_are_capped() {
        let l = list();
        let base = op(Address::repeat_byte(0x99), single(l.registry, release()));
        let mut op = base.clone();
        op.call_gas_limit = Some(U256::from(MAX_CALL_GAS));
        op.max_fee_per_gas = Some(U256::from(1_000_000_000u64)); // 1 gwei: 0.003 ETH at most
        l.check(&op).unwrap();
        op.call_gas_limit = None;
        op.max_fee_per_gas = Some(U256::from(MAX_FEE_PER_GAS));
        l.check(&op).unwrap();
        let too_much: [fn(&mut UserOpView); 7] = [
            |o| o.call_gas_limit = Some(U256::from(MAX_CALL_GAS + 1)),
            |o| o.verification_gas_limit = Some(U256::from(MAX_VERIFICATION_GAS + 1)),
            |o| o.pre_verification_gas = Some(U256::from(MAX_PRE_VERIFICATION_GAS + 1)),
            |o| o.paymaster_verification_gas_limit = Some(U256::from(MAX_PAYMASTER_GAS + 1)),
            |o| o.paymaster_post_op_gas_limit = Some(U256::MAX),
            |o| o.max_fee_per_gas = Some(U256::from(MAX_FEE_PER_GAS + 1)),
            |o| o.max_priority_fee_per_gas = Some(U256::MAX),
        ];
        for f in too_much {
            let mut op = base.clone();
            f(&mut op);
            assert_eq!(l.check(&op), Err("gas limits or fees above the sponsorship caps"));
        }
        // Within every field cap but too expensive in total: 57M gas at 10 gwei = 0.57 ETH.
        let mut op = base.clone();
        op.call_gas_limit = Some(U256::from(MAX_CALL_GAS));
        op.verification_gas_limit = Some(U256::from(MAX_VERIFICATION_GAS));
        op.pre_verification_gas = Some(U256::from(MAX_PRE_VERIFICATION_GAS));
        op.max_fee_per_gas = Some(U256::from(MAX_FEE_PER_GAS));
        assert_eq!(l.check(&op), Err("gas limits or fees above the sponsorship caps"));
        // Base-like fees: a large pre-verification gas at a tiny gas price is cheap and fine.
        op.max_fee_per_gas = Some(U256::from(2_000_000u64));
        l.check(&op).unwrap();
        // JSON as the app sends it: hex quantities.
        let json = serde_json::json!({
            "sender": Address::repeat_byte(0x99), "callData": base.call_data,
            "callGasLimit": "0x2dc6c1", "maxFeePerGas": "0x1"
        });
        let parsed: UserOpView = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.call_gas_limit, Some(U256::from(MAX_CALL_GAS + 1)));
        assert!(l.check(&parsed).is_err());
    }

    /// M-3: MultiSend batches are capped, and a hostile length cannot overflow.
    #[test]
    fn batches_are_capped_and_lengths_checked() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let call = (l.registry, release());
        l.check(&op(me, batch(&vec![call.clone(); MAX_BATCH_CALLS]))).unwrap();
        assert_eq!(
            l.check(&op(me, batch(&vec![call.clone(); MAX_BATCH_CALLS + 1]))),
            Err("batch is empty or too long")
        );
        assert_eq!(l.check(&op(me, batch(&[]))), Err("batch is empty or too long"));

        let mut hostile = pack(std::slice::from_ref(&call));
        hostile[53..85].copy_from_slice(&U256::from(u64::MAX).to_be_bytes::<32>());
        assert_eq!(l.check(&op(me, multisend(hostile))), Err("truncated multiSend data"));
    }
}
