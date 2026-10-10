//! The paymaster allowlist (docs/INTERFACES.md § 4.1): which UserOps we sponsor.
//!
//! A UserOp is sponsored only if (a) its account deployment, if any, is a Safe v1.4.1 created
//! by the canonical proxy factory, and (b) every call it makes is on the list below, with no
//! ETH value anywhere. The smart account therefore never needs (or receives) ETH, and the
//! paymaster cannot be used for anything but Zecured flows.

use alloy::{
    primitives::{Address, Bytes, U256, address},
    sol,
    sol_types::SolCall,
};
use anyhow::{Result, bail, ensure};
use serde::Deserialize;

/// Canonical Safe v1.4.1 deployments (same on every chain), as used by permissionless.js.
pub const SAFE_PROXY_FACTORY: Address = address!("4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67");
pub const SAFE_SINGLETON: Address = address!("41675C099F32341bf84BFc5382aF534df5C7461a");
pub const MULTI_SEND_CALL_ONLY: Address = address!("9641d764fc13c8B624c04430C7356C1C7C8102e2");

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

    function setAuthorization(address authorized, bool newIsAuthorized);
    function borrow(MarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, address receiver);
    function repay(MarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, bytes data);
    function approve(address spender, uint256 amount);
    function transfer(address to, uint256 amount);
    function requestRelease(bytes32 vaultId, bytes32 destCommitment);
    function invalidateNonce(uint256 nonce);
}

/// Deployment-specific parameters of the allowlist.
#[derive(Clone, Debug, Deserialize)]
pub struct Allowlist {
    pub morpho: Address,
    pub registry: Address,
    pub adapter: Address,
    pub usdc: Address,
    pub loan_token: Address,
    pub collateral_token: Address,
    pub oracle: Address,
    pub irm: Address,
    pub lltv: U256,
}

/// The fields of a v0.7 UserOp the allowlist looks at.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserOpView {
    pub sender: Address,
    #[serde(default)]
    pub factory: Option<Address>,
    #[serde(default)]
    pub factory_data: Option<Bytes>,
    pub call_data: Bytes,
}

impl Allowlist {
    fn is_our_market(&self, m: &MarketParams) -> bool {
        m.loanToken == self.loan_token
            && m.collateralToken == self.collateral_token
            && m.oracle == self.oracle
            && m.irm == self.irm
            && m.lltv == self.lltv
    }

    /// Decides whether to sponsor `op`.
    pub fn check(&self, op: &UserOpView) -> Result<()> {
        if let Some(factory) = op.factory {
            ensure!(factory == SAFE_PROXY_FACTORY, "unknown account factory");
            let data = op.factory_data.as_ref().map(|d| d.as_ref()).unwrap_or_default();
            let call = createProxyWithNonceCall::abi_decode(data)?;
            ensure!(call.singleton == SAFE_SINGLETON, "unexpected Safe singleton");
        }
        let data = op.call_data.as_ref();
        let (to, value, inner, operation) = if let Ok(c) = executeUserOpCall::abi_decode(data) {
            (c.to, c.value, c.data, c.operation)
        } else if let Ok(c) = executeUserOpWithErrorStringCall::abi_decode(data) {
            (c.to, c.value, c.data, c.operation)
        } else {
            bail!("callData is not a Safe 4337 execution");
        };
        ensure!(value.is_zero(), "no ETH value allowed");
        match operation {
            OPERATION_CALL => self.check_call(op.sender, to, &inner),
            OPERATION_DELEGATECALL if to == MULTI_SEND_CALL_ONLY => {
                let batch = multiSendCall::abi_decode(&inner)?;
                for (op_kind, target, value, data) in unpack_multisend(&batch.transactions)? {
                    ensure!(op_kind == OPERATION_CALL, "batched delegatecall not allowed");
                    ensure!(value.is_zero(), "no ETH value allowed");
                    self.check_call(op.sender, target, &data)?;
                }
                Ok(())
            }
            _ => bail!("operation not allowed"),
        }
    }

    fn check_call(&self, sender: Address, to: Address, data: &[u8]) -> Result<()> {
        ensure!(data.len() >= 4, "missing selector");
        let selector: [u8; 4] = data[..4].try_into().expect("checked length");
        if to == self.morpho {
            if selector == setAuthorizationCall::SELECTOR {
                let c = setAuthorizationCall::abi_decode(data)?;
                ensure!(
                    c.authorized == self.registry || c.authorized == self.adapter,
                    "authorization only for the registry or the adapter"
                );
                return Ok(());
            }
            if selector == borrowCall::SELECTOR {
                let c = borrowCall::abi_decode(data)?;
                ensure!(self.is_our_market(&c.marketParams), "not the Zecured market");
                ensure!(c.onBehalf == sender, "borrow must be for the account itself");
                return Ok(());
            }
            if selector == repayCall::SELECTOR {
                let c = repayCall::abi_decode(data)?;
                ensure!(self.is_our_market(&c.marketParams), "not the Zecured market");
                ensure!(c.onBehalf == sender, "repay must be for the account itself");
                ensure!(c.data.is_empty(), "repay callbacks not allowed");
                return Ok(());
            }
        } else if to == self.usdc {
            if selector == approveCall::SELECTOR {
                ensure!(approveCall::abi_decode(data)?.spender == self.morpho, "approve only to Morpho");
                return Ok(());
            }
            if selector == transferCall::SELECTOR {
                return Ok(());
            }
        } else if to == self.registry && selector == requestReleaseCall::SELECTOR {
            requestReleaseCall::abi_decode(data)?;
            return Ok(());
        } else if to == self.adapter && selector == invalidateNonceCall::SELECTOR {
            invalidateNonceCall::abi_decode(data)?;
            return Ok(());
        }
        bail!("call not on the allowlist")
    }
}

/// MultiSend packing: operation (1) | to (20) | value (32) | data length (32) | data.
fn unpack_multisend(mut packed: &[u8]) -> Result<Vec<(u8, Address, U256, Vec<u8>)>> {
    const HEADER: usize = 1 + 20 + 32 + 32;
    let mut out = vec![];
    while !packed.is_empty() {
        ensure!(packed.len() >= HEADER, "truncated multiSend entry");
        let op = packed[0];
        let to = Address::from_slice(&packed[1..21]);
        let value = U256::from_be_slice(&packed[21..53]);
        let len = usize::try_from(U256::from_be_slice(&packed[53..85])).map_err(|_| anyhow::anyhow!("length overflow"))?;
        ensure!(packed.len() >= HEADER + len, "truncated multiSend data");
        out.push((op, to, value, packed[HEADER..HEADER + len].to_vec()));
        packed = &packed[HEADER + len..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;

    fn list() -> Allowlist {
        Allowlist {
            morpho: Address::repeat_byte(0x11),
            registry: Address::repeat_byte(0x22),
            adapter: Address::repeat_byte(0x33),
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

    fn single(to: Address, data: Vec<u8>) -> Bytes {
        executeUserOpCall {
            to,
            value: U256::ZERO,
            data: data.into(),
            operation: OPERATION_CALL,
        }
        .abi_encode()
        .into()
    }

    fn batch(calls: &[(Address, Vec<u8>)]) -> Bytes {
        let mut packed = vec![];
        for (to, data) in calls {
            packed.push(OPERATION_CALL);
            packed.extend_from_slice(to.as_slice());
            packed.extend_from_slice(&[0u8; 32]);
            packed.extend_from_slice(&U256::from(data.len()).to_be_bytes::<32>());
            packed.extend_from_slice(data);
        }
        executeUserOpCall {
            to: MULTI_SEND_CALL_ONLY,
            value: U256::ZERO,
            data: multiSendCall { transactions: packed.into() }.abi_encode().into(),
            operation: OPERATION_DELEGATECALL,
        }
        .abi_encode()
        .into()
    }

    fn op(sender: Address, call_data: Bytes) -> UserOpView {
        UserOpView {
            sender,
            factory: None,
            factory_data: None,
            call_data,
        }
    }

    #[test]
    fn first_borrow_batch_is_sponsored() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let calls = vec![
            (l.morpho, setAuthorizationCall { authorized: l.registry, newIsAuthorized: true }.abi_encode()),
            (l.morpho, setAuthorizationCall { authorized: l.adapter, newIsAuthorized: true }.abi_encode()),
            (
                l.morpho,
                borrowCall { marketParams: market(&l), assets: U256::from(1_000_000u64), shares: U256::ZERO, onBehalf: me, receiver: me }
                    .abi_encode(),
            ),
        ];
        l.check(&op(me, batch(&calls))).unwrap();
    }

    #[test]
    fn repay_and_release_batch_is_sponsored() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let calls = vec![
            (l.usdc, approveCall { spender: l.morpho, amount: U256::MAX }.abi_encode()),
            (
                l.morpho,
                repayCall { marketParams: market(&l), assets: U256::ZERO, shares: U256::from(5u8), onBehalf: me, data: Bytes::new() }
                    .abi_encode(),
            ),
            (l.registry, requestReleaseCall { vaultId: B256::repeat_byte(1), destCommitment: B256::repeat_byte(2) }.abi_encode()),
        ];
        l.check(&op(me, batch(&calls))).unwrap();
    }

    #[test]
    fn everything_else_is_refused() {
        let l = list();
        let me = Address::repeat_byte(0x99);
        let stranger = Address::repeat_byte(0xab);
        // authorizing anyone else
        assert!(l.check(&op(me, single(l.morpho, setAuthorizationCall { authorized: stranger, newIsAuthorized: true }.abi_encode()))).is_err());
        // borrowing on behalf of someone else
        let b = borrowCall { marketParams: market(&l), assets: U256::from(1u8), shares: U256::ZERO, onBehalf: stranger, receiver: me };
        assert!(l.check(&op(me, single(l.morpho, b.abi_encode()))).is_err());
        // another market
        let mut m = market(&l);
        m.lltv = U256::from(1u8);
        let b = borrowCall { marketParams: m, assets: U256::from(1u8), shares: U256::ZERO, onBehalf: me, receiver: me };
        assert!(l.check(&op(me, single(l.morpho, b.abi_encode()))).is_err());
        // approving a non-Morpho spender
        assert!(l.check(&op(me, single(l.usdc, approveCall { spender: stranger, amount: U256::MAX }.abi_encode()))).is_err());
        // arbitrary contract call
        assert!(l.check(&op(me, single(stranger, vec![1, 2, 3, 4]))).is_err());
        // ETH value
        let with_value = executeUserOpCall {
            to: l.usdc,
            value: U256::from(1u8),
            data: transferCall { to: stranger, amount: U256::from(1u8) }.abi_encode().into(),
            operation: OPERATION_CALL,
        };
        assert!(l.check(&op(me, with_value.abi_encode().into())).is_err());
        // delegatecall to something other than MultiSendCallOnly
        let dc = executeUserOpCall { to: stranger, value: U256::ZERO, data: Bytes::new(), operation: OPERATION_DELEGATECALL };
        assert!(l.check(&op(me, dc.abi_encode().into())).is_err());
        // unknown factory
        let mut deploy = op(me, single(l.usdc, transferCall { to: stranger, amount: U256::from(1u8) }.abi_encode()));
        deploy.factory = Some(stranger);
        assert!(l.check(&deploy).is_err());
    }
}
