//! ZecuredSwap's ABI as far as the maker bot and the CLI use it (contracts/src/interfaces/IZecuredSwap.sol;
//! the full ABI is contracts/abi/ZecuredSwap.json).

use alloy::sol;

sol! {
    /// The maker's signed offer. EIP-712, domain "ZecuredSwap" "1":
    /// SwapQuote(address maker,address taker,uint8 side,uint64 zatoshi,uint256 usdc,bytes32 makerReceiverCommitment,uint32 payWindow,uint64 quoteDeadline,uint256 nonce)
    #[derive(Debug, PartialEq, Eq)]
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

    #[sol(rpc)]
    interface IZecuredSwap {
        /// `status`: 0 None, 1 Open, 2 Settled, 3 Refunded (`OrderStatus`).
        #[derive(Debug, PartialEq, Eq)]
        struct Order {
            address maker;
            uint64 zatoshi;
            uint8 side;
            uint8 status;
            address taker;
            uint64 payDeadline;
            uint256 usdc;
            bytes32 receiverCommitment;
        }

        #[derive(Debug, PartialEq, Eq)]
        struct PendingSettlement {
            bytes32 anchor;
            bytes32 versionId;
            uint64 readyHeight;
            uint64 readyTime;
        }

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

        function open(SwapQuote q, bytes makerSig, bytes32 takerReceiverCommitment) external returns (bytes32 orderId);
        function proveSettlement(bytes32 orderId, bytes journal, bytes seal, bytes32 imageId) external;
        function finalizeSettlement(bytes32 orderId) external;
        function dropSettlement(bytes32 orderId) external;
        function refund(bytes32 orderId) external;

        function order(bytes32 orderId) external view returns (Order memory);
        function pendingSettlement(bytes32 orderId) external view returns (PendingSettlement memory);
        function settlementStale(bytes32 orderId) external view returns (bool);
        function orderIdOf(address maker, uint256 nonce) external view returns (bytes32);
        function hashSwapQuote(SwapQuote q) external view returns (bytes32);
        function minConfirmationWorkFor(uint64 zatoshi) external view returns (uint256);
        function lockedUsdc() external view returns (uint256);
        function usdc() external view returns (address);
        function relay() external view returns (address);
        function versions() external view returns (address);
        function verifier() external view returns (address);
        function maxOrderUsdc() external view returns (uint256);
        function maxOrderZat() external view returns (uint64);
        function minPayWindow() external view returns (uint32);
        function maxPayWindow() external view returns (uint32);
        function finalizeDepth() external view returns (uint64);
        function finalizeDelay() external view returns (uint64);
        function feeBps() external view returns (uint16);
    }

    /// The escrowed token, as far as the parties need it.
    #[sol(rpc)]
    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
        function balanceOf(address owner) external view returns (uint256);
        function decimals() external view returns (uint8);
    }

    /// The ZEC/USD price in Morpho's scale (USDC units per zatoshi, times 1e36): `price()` of any
    /// Morpho oracle; `priceHigh()` of ZecuredPriceFeed (the higher fresh source and when it was
    /// last fresh).
    #[sol(rpc)]
    interface IZecPrice {
        function price() external view returns (uint256);
        function priceHigh() external view returns (uint256 price, uint256 updatedAt);
    }
}
