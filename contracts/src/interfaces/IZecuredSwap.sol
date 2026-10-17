// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorkTier} from "./v3/IZecuredIntake.sol";

// `SwapQuote.side`, from the taker's side. Buy: the taker buys ZEC (locks USDC; the maker pays the ZEC and proves it).
// Sell: the taker sells ZEC (the maker locks USDC; the taker pays the ZEC and proves it).
uint8 constant SIDE_BUY = 0;
uint8 constant SIDE_SELL = 1;

/// @notice A maker's signed offer (EIP-712, domain "ZecuredSwap" "1"):
///         SwapQuote(address maker,address taker,uint8 side,uint64 zatoshi,uint256 usdc,bytes32 makerReceiverCommitment,
///         uint32 payWindow,uint64 quoteDeadline,uint256 nonce)
/// @param maker Signs the quote (an EOA, or a contract through ERC-1271); its nonce names the order.
/// @param taker The only account that may open the quote, or zero: anyone may.
/// @param side `SIDE_BUY` or `SIDE_SELL`.
/// @param zatoshi The ZEC the payer pays, in zatoshi: the payment note must carry at least this much.
/// @param usdc The USDC locked and released, in USDC units (6 decimals).
/// @param makerReceiverCommitment Sell: `keccak256(salt || receiver)` of the maker's Ironwood receiver, which the
///        taker pays; Buy: zero (the taker gives its own commitment to `open`).
/// @param payWindow Seconds from `open` within which the payment must be proven (`payDeadline`).
/// @param quoteDeadline The last second the quote may be opened.
/// @param nonce The maker's; spent by `open`.
struct SwapQuote {
    address maker;
    address taker;
    uint8 side;
    uint64 zatoshi;
    uint256 usdc;
    bytes32 makerReceiverCommitment;
    uint32 payWindow;
    uint64 quoteDeadline;
    uint256 nonce;
}

/// @notice `None` (no such order), `Open` (USDC locked, payment not yet proven and finalized), `Settled` (the USDC went
///         to the payer's counterparty: the maker on Buy, the taker on Sell), `Refunded` (back to whoever locked it).
enum OrderStatus {
    None,
    Open,
    Settled,
    Refunded
}

/// @notice An order: a quote opened by its taker. `receiverCommitment` is the commitment the payment must pay (the
///         taker's on Buy, the maker's on Sell).
struct Order {
    address maker;
    uint64 zatoshi;
    uint8 side;
    OrderStatus status;
    address taker;
    uint64 payDeadline;
    uint256 usdc;
    bytes32 receiverCommitment;
}

/// @notice K3: a proven payment waiting for the relay to move on (`readyHeight`, `readyTime`), with the Swap version
///         whose guest proved it (R-14: the record is dropped if that version stops being active).
struct PendingSettlement {
    bytes32 anchor;
    bytes32 versionId;
    uint64 readyHeight;
    uint64 readyTime;
}

/// @notice The payment guest's journal (tier2/payment-statement, 192 bytes):
///         abi.encode(uint8 kind = 1, uint32 branchId, bytes32 anchor, bytes32 orderId, bytes32 receiverCommitment,
///         uint64 zatoshi). `anchor` is the header the payment's block is proven under; `zatoshi` the note's value.
struct PaymentJournal {
    uint8 kind;
    uint32 branchId;
    bytes32 anchor;
    bytes32 orderId;
    bytes32 receiverCommitment;
    uint64 zatoshi;
}

/// @notice Constructor arguments of ZecuredSwap.
/// @param usdc The escrowed token (USDC, 6 decimals; any fee-on-transfer token is refused at `open`).
/// @param verifier RISC Zero's verifier router: the one `versions` is built on (checked).
/// @param versions ZecuredVersions: payment proofs verify only through an active `Swap` version (router selector and
///        guest image id), whose branch ids a journal must name.
/// @param relay The Zcash header relay of the network the Swap versions' guests prove.
/// @param workTiers Work on top of a payment's anchor by the order's amount, ascending (at most four); `minWork`
///        never decreases.
/// @param finalizeDepth Headers the relay must add after a proof before `finalizeSettlement` (K3).
/// @param finalizeDelay Seconds after a proof before `finalizeSettlement` (K3).
/// @param maxOrderUsdc Largest order in USDC units.
/// @param maxOrderZat Largest order in zatoshi; within the largest work tier.
/// @param minPayWindow Shortest `payWindow` a quote may give: a payer must have time to pay and prove.
/// @param maxPayWindow Longest `payWindow`: how long a locker's USDC may wait for a payment.
/// @param feeBps Protocol fee on the released USDC (0 on the judge deployment), at most `MAX_FEE_BPS`.
/// @param feeRecipient Receives the fee (required when `feeBps` is not zero).
struct SwapConfig {
    address usdc;
    address verifier;
    address versions;
    address relay;
    WorkTier[] workTiers;
    uint64 finalizeDepth;
    uint64 finalizeDelay;
    uint256 maxOrderUsdc;
    uint64 maxOrderZat;
    uint32 minPayWindow;
    uint32 maxPayWindow;
    uint16 feeBps;
    address feeRecipient;
}

/// @title IZecuredSwap
/// @notice Zecured Swap: USDC escrowed on Base is released by a zero-knowledge proof of a shielded ZEC payment, made by
///         whoever paid. No custodian, no signer, no bond, no wrapped asset: what Base trusts is the Zcash header
///         relay's proof of work (work tiers by amount, then K3), RISC Zero's verifier and the payment guest
///         (tier2/payment-statement).
/// @dev Flow. The taker opens a maker's signed quote (`open`); the USDC is locked (Buy: the taker's; Sell: the maker's).
///      The recipient's Ironwood receiver and salt travel off-chain (Buy: the taker gives them to the maker); Base only
///      sees `keccak256(salt || receiver)`. The payer pays one note with the memo "ZSWP" || orderId, proves it
///      (`proveSettlement`, by `payDeadline`), and once the relay has moved on anyone finalizes (`finalizeSettlement`):
///      the USDC goes to the payer (Buy: the maker; Sell: the taker), less the fee. An order nobody proved in time is
///      refunded to whoever locked it (`refund`). A proof whose anchor left the relay's best chain, or whose Swap
///      version was removed, is dropped (`dropSettlement`) and the order is open again.
///      Deliberately absent: a taker's cancel (the maker may already have paid) and a maker bond (the delivery of the
///      receiver cannot be proven on Base; a maker that never pays only costs the taker time).
///      `orderId = keccak256(abi.encode(block.chainid, address(this), maker, nonce))`: a journal names one order of one
///      contract on one chain.
interface IZecuredSwap {
    event OrderOpened(
        bytes32 indexed orderId,
        address indexed maker,
        address indexed taker,
        uint8 side,
        uint64 zatoshi,
        uint256 usdc,
        bytes32 receiverCommitment,
        uint64 payDeadline
    );
    event SettlementPending(bytes32 indexed orderId, bytes32 anchor, uint256 readyHeight, uint256 readyTime);
    event OrderSettled(bytes32 indexed orderId);
    event SettlementDropped(bytes32 indexed orderId);
    event OrderRefunded(bytes32 indexed orderId);

    error ZeroAddress();
    error InvalidConfig();
    error InvalidSide(uint8 side);
    error InvalidAmount();
    error OrderTooLarge(uint256 usdc, uint64 zatoshi);
    error PayWindowOutOfRange(uint32 payWindow);
    error QuoteExpired(uint64 quoteDeadline);
    error NotQuoteTaker(address caller);
    error MissingReceiverCommitment();
    error UnexpectedReceiverCommitment();
    error NonceUsed(bytes32 orderId);
    error BadMakerSignature();
    error TransferMismatch(uint256 received, uint256 expected);
    error OrderNotOpen(bytes32 orderId);
    error PayDeadlinePassed(bytes32 orderId, uint64 payDeadline);
    error PayDeadlineNotPassed(bytes32 orderId, uint64 payDeadline);
    error InvalidJournalLength(uint256 length);
    error InvalidJournalKind(uint8 kind);
    error BranchNotAllowed(uint32 branchId);
    error JournalOrderMismatch(bytes32 journalOrderId);
    error ReceiverMismatch(bytes32 journalReceiverCommitment);
    error AmountTooLow(uint64 paid, uint64 due);
    error AnchorNotConfirmed(bytes32 anchor, uint256 work);
    error AmountAboveLargestTier(uint64 zatoshi);
    error SettlementAlreadyPending(bytes32 orderId);
    error NoPendingSettlement(bytes32 orderId);
    error SettlementNotReady(bytes32 orderId, uint256 readyHeight, uint256 readyTime);
    error SettlementStillValid(bytes32 orderId);
    error SettlementIsPending(bytes32 orderId);
    error SwapVersionNotActive(bytes32 versionId);

    /// @notice Opens `q` as an order of `msg.sender` (the taker): checks the maker's signature (ERC-1271 for a contract
    ///         maker), spends the maker's nonce, locks the USDC (Buy: from the taker; Sell: from the maker, who approved
    ///         this contract) and starts the pay window. Buy: `takerReceiverCommitment` is the taker's
    ///         `keccak256(salt || receiver)` and `q.makerReceiverCommitment` must be zero; Sell: the reverse.
    function open(SwapQuote calldata q, bytes calldata makerSig, bytes32 takerReceiverCommitment)
        external
        returns (bytes32 orderId);

    /// @notice Anyone, by `payDeadline`: a payment proof of an open order (an active Swap version's seal, a branch id it
    ///         allows, the order's id and receiver commitment, at least its zatoshi, an anchor on the relay's best chain
    ///         with its tier's work on top). The settlement is then pending (K3). A pending settlement that is no
    ///         longer valid (`settlementStale`) is dropped first; a valid one is never replaced.
    function proveSettlement(bytes32 orderId, bytes calldata journal, bytes calldata seal, bytes32 imageId) external;

    /// @notice Anyone, once the relay reached `readyHeight` and `readyTime` passed (also after `payDeadline`): the
    ///         anchor still on the best chain with its work, the proof's Swap version still active. Releases the USDC
    ///         to the payer, less the fee.
    function finalizeSettlement(bytes32 orderId) external;

    /// @notice Anyone: drops a pending settlement whose anchor left the relay's best chain or whose Swap version is no
    ///         longer active (R-14). The order is open again (provable again until `payDeadline`).
    function dropSettlement(bytes32 orderId) external;

    /// @notice Anyone, after `payDeadline`, for an open order without a valid pending settlement (a stale one is
    ///         dropped first): the USDC goes back to whoever locked it.
    function refund(bytes32 orderId) external;

    function order(bytes32 orderId) external view returns (Order memory);
    function pendingSettlement(bytes32 orderId) external view returns (PendingSettlement memory);
    /// @notice Whether the order's pending settlement exists and may be dropped.
    function settlementStale(bytes32 orderId) external view returns (bool);
    /// @notice The id `open` gives the order of `maker`'s quote `nonce`.
    function orderIdOf(address maker, uint256 nonce) external view returns (bytes32);
    /// @notice The EIP-712 digest the maker signs for `q`.
    function hashSwapQuote(SwapQuote calldata q) external view returns (bytes32);
    /// @notice The work a payment of an order of `zatoshi` needs on top of its anchor.
    function minConfirmationWorkFor(uint64 zatoshi) external view returns (uint256);
    function workTiers() external view returns (WorkTier[] memory);
    /// @notice The USDC of every open order (`Σ usdc` over `Open` orders).
    function lockedUsdc() external view returns (uint256);
    function domainSeparator() external view returns (bytes32);
    function usdc() external view returns (address);
    function verifier() external view returns (address);
    function versions() external view returns (address);
    function relay() external view returns (address);
    function finalizeDepth() external view returns (uint64);
    function finalizeDelay() external view returns (uint64);
    function maxOrderUsdc() external view returns (uint256);
    function maxOrderZat() external view returns (uint64);
    function minPayWindow() external view returns (uint32);
    function maxPayWindow() external view returns (uint32);
    function feeBps() external view returns (uint16);
    function feeRecipient() external view returns (address);
}
