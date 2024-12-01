//! The program is ABI-equivalent with Solidity, which means you can call it from both Solidity and Rust. To do this, run `cargo stylus export-abi`.

// TODO - natspec

// Allow `cargo stylus export-abi` to generate a main function.
#![cfg_attr(not(feature = "export-abi"), no_main)]

extern crate alloc; // Link alloc to access Vec

use alloy_sol_types::sol; // Define errors and interfaces
use stylus_sdk::{
    alloy_primitives::{U256, Address},
    prelude::*, // Contains common traits and macros.
    block,      // Includes block::timestamp
    console,    // For Debug purposes. Todo remove
    msg,        // Access msg::sender
    contract,   // Access global smart contract info
    evm         // Events
};

sol_interface! {
    interface IERC20 {
        function transfer(address, uint256) external returns (bool);
        function transferFrom(address, address, uint256) external returns (bool);
    }
}

// Define some persistent storage using the Solidity ABI.
// `TokenSaleWithTokenizedVesting` will be the entrypoint.
sol_storage! {
    #[entrypoint]
    pub struct TokenSaleWithTokenizedVesting {
        bool initialized;                               // Required before contract usage
        address owner;                                  // Smart contract manager 
        address token;                                  // Token being purchased
        address currency;                               // Payment currency for token
        uint256 price_per_token;                        // Price per token being purchased
        uint256 total_tokens_available;                 // Total number of tokens available for purchase
        uint256 total_vesting_length_in_seconds;        // Non-zero if tokens must be vested to buyer
        uint256 percentage_unlocked_on_purchase;        // If tokens need to be vested, what percentage is instantly redeemable 
        address nft_claim;                              // Address of the NFT contract that can tokenise vesting
        uint256 total_tokens_purchased;                 // Total number of tokens purchased accross all users
        mapping(address => uint256) tokens_purchased;   // Tracking how many tokens a user has bought
        mapping(address => uint256) tokens_purchased_at;// Tracking the timestamp when a user purchased their tokens
        mapping(address => uint256) tokens_claimed;     // Total number of vested tokens that have already been claimed
        mapping(address => uint256) tokens_claimed_at;  // Last timestamp of claim or zero if not been claimed yet
        mapping(address => uint256) nft_claim_token_id; // If enabled, the token ID of the NFT that is allowed to claim the vested tokens
    }
}

// Declare events and Solidity error types
sol! {
    /// ******
    /// Errors
    /// ******

    error AlreadyInitialized();
    error OnlyOwner();
    error ZeroValueArgumentInjected();
    error InvalidPercentage();
    error VestingLengthTooShort();
    error VestingLengthTooLong();
    error OnlyOnePurchase();
    error SoldOut();
    error VestingNotEnabled();
    error NoTokensVested();
    error AlreadyTokenized();

    /// ******
    /// Events
    /// ******
    event TokensPurchased(address indexed user, uint256 amount);
    event TokenizedVestingEnabled(address indexed user, uint256 indexed nft_token_id);
}

#[derive(SolidityError)]
pub enum Errors {
    OnlyOwner(OnlyOwner),
    AlreadyInitialized(AlreadyInitialized),
    ZeroValueArgumentInjected(ZeroValueArgumentInjected),
    InvalidPercentage(InvalidPercentage),
    VestingLengthTooShort(VestingLengthTooShort),
    VestingLengthTooLong(VestingLengthTooLong),
    OnlyOnePurchase(OnlyOnePurchase),
    SoldOut(SoldOut),
    VestingNotEnabled(VestingNotEnabled),
    NoTokensVested(NoTokensVested),
    AlreadyTokenized(AlreadyTokenized)
}

// 100% defined to 3 decimal places
const ONE_HUNDRED_PERCENT: i32 = 100_000;

// One day defined in seconds as the minimum vesting length if applicable
const MIN_VESTING_LENGTH: i32 = 86_400;

// 365 days defined in seconds as the maximum vesting length if applicable
const MAX_VESTING_LENGTH: i32 = 31_536_000;

/// External methods for `TokenSaleWithTokenizedVesting`
#[public]
impl TokenSaleWithTokenizedVesting {

    /// ******
    /// Init
    /// ******

    pub fn init(
        &mut self,
        token: Address,
        currency: Address,
        price_per_token: U256,
        total_tokens_available: U256,
        total_vesting_length_in_seconds: U256,
        percentage_unlocked_on_purchase: U256,
        nft_claim: Address,
    ) -> Result<(), Errors> {
        // Perform required validation
        self.validate_initialization()?;
        self.validate_price_per_token(price_per_token)?;
        self.validate_address(token)?;
        self.validate_address(currency)?;
        self.validate_total_tokens_for_sale(total_tokens_available)?;
        self.validate_vesting_length(total_vesting_length_in_seconds)?;
        self.validate_percentage_unlocked(total_vesting_length_in_seconds, percentage_unlocked_on_purchase)?;
        self.validate_address(nft_claim)?;

        // Setup the smart contract by configuring storage
        self.initialized.set(true);
        self.owner.set(msg::sender());
        self.token.set(token);
        self.currency.set(currency);
        self.price_per_token.set(price_per_token);
        self.total_tokens_available.set(total_tokens_available);
        self.total_vesting_length_in_seconds.set(total_vesting_length_in_seconds);
        self.percentage_unlocked_on_purchase.set(percentage_unlocked_on_purchase);
        self.nft_claim.set(nft_claim);

        Ok(())
    }

    /// ******
    /// User
    /// ******

    pub fn purchase_tokens(&mut self, amount: U256) -> Result<(), Errors> {
        // For simplicity on vesting, we only let the address buy a token allocation once. They can create other addresses if they want more
        let tokens_purchased_by_user = self.tokens_purchased.get(msg::sender());
        if tokens_purchased_by_user > U256::ZERO {
            return Err(Errors::OnlyOnePurchase(OnlyOnePurchase {}))
        }

        // Check if global limit has been reached
        let total_tokens_purchased = self.total_tokens_purchased.get();
        if total_tokens_purchased + amount > self.total_tokens_available.get() {
            return Err(Errors::SoldOut(SoldOut {}))
        }

        // Record how many tokens user is buying and when they bought it
        let mut tokens_purchased = self.tokens_purchased.setter(msg::sender());
        let mut tokens_purchased_at = self.tokens_purchased_at.setter(msg::sender());
        tokens_purchased.set(amount);
        tokens_purchased_at.set(U256::from(block::timestamp()));
        self.total_tokens_purchased.set(total_tokens_purchased + amount);

        // Take payment for the tokens
        let cost = amount * self.price_per_token.get();
        let _ = IERC20::from(IERC20 {address: self.currency.get()}).transfer_from(
            self,
            msg::sender(), 
            contract::address(),
            cost
        );

        // Log the purchase and conclude the transaction
        evm::log(TokensPurchased {
            user: msg::sender(),
            amount
        });

        Ok(())
    }

    pub fn enable_tokenized_vesting(&mut self, token_id: U256) -> Result<(), Errors> {
        // Validate whether it is possible to enable tokenized vesting
        if self.total_vesting_length_in_seconds.get() == U256::ZERO {
            return Err(Errors::VestingNotEnabled(VestingNotEnabled {}))
        }

        let tokens_purchased_by_user = self.tokens_purchased.get(msg::sender());
        if tokens_purchased_by_user == U256::ZERO {
            return Err(Errors::NoTokensVested(NoTokensVested {}))
        }

        let nft_claim_token_id = self.nft_claim_token_id.get(msg::sender());
        if nft_claim_token_id != U256::ZERO {
            return Err(Errors::AlreadyTokenized(AlreadyTokenized {}))
        }

        if token_id == U256::ZERO {
            return Err(Errors::ZeroValueArgumentInjected(ZeroValueArgumentInjected {}))
        }

        // Record the NFT that tokenized the vesting so that its owner can start claiming tokens
        let mut claim_token_id_setter = self.nft_claim_token_id.setter(msg::sender());
        claim_token_id_setter.set(token_id);

        // Log the vesting being enabled and conclude the transaction
        evm::log(TokenizedVestingEnabled {
            user: msg::sender(),
            nft_token_id: token_id
        });
        
        Ok(())
    }

    /// ******
    /// Owner
    /// ******

    pub fn update_price_per_token(&mut self, new_price_per_token: U256) -> Result<(), Errors> {
        self.validate_sender_is_owner()?;
        self.validate_price_per_token(new_price_per_token)?;
        Ok(())
    }

    /// ******
    /// View
    /// ******

    pub fn owner(&self) -> Address {
        self.owner.get()
    }

}

// Internal methods for `TokenSaleWithTokenizedVesting`
impl TokenSaleWithTokenizedVesting {
    // Ensure we are not already initialized
    pub fn validate_initialization(&self) -> Result<(), Errors> {
        if self.initialized.get() {
            return Err(Errors::AlreadyInitialized(AlreadyInitialized {}))
        } 

        Ok(())
    }

    // Ensure sender is owner
    pub fn validate_sender_is_owner(&self) -> Result<(), Errors> {
        if msg::sender() != self.owner.get() {
            return Err(Errors::OnlyOwner(OnlyOwner {}))
        }
        
        Ok(())
    }

    // Ensure a zero price is not supplied
    pub fn validate_price_per_token(&self, price_per_token: U256) -> Result<(), Errors> {
        if price_per_token == U256::ZERO {
            return Err(Errors::ZeroValueArgumentInjected(ZeroValueArgumentInjected {}))
        }

        Ok(())
    }

    // Ensure a zero value is not supplied
    pub fn validate_address(&self, value: Address) -> Result<(), Errors> {
        if value == Address::ZERO {
            return Err(Errors::ZeroValueArgumentInjected(ZeroValueArgumentInjected {}))
        }

        Ok(())
    }

    pub fn validate_total_tokens_for_sale(&self, total_tokens: U256) -> Result<(), Errors> {
        if total_tokens == U256::ZERO {
            return Err(Errors::ZeroValueArgumentInjected(ZeroValueArgumentInjected {}))
        }

        Ok(())
    }

    // Ensure vesting length is not zero and a sensible length
    pub fn validate_vesting_length(&self, vesting_length: U256) -> Result<(), Errors> {
        if vesting_length != U256::ZERO {
            if vesting_length < U256::from(MIN_VESTING_LENGTH) {
                return Err(Errors::VestingLengthTooShort(VestingLengthTooShort {}))
            }
    
            if vesting_length > U256::from(MAX_VESTING_LENGTH) {
                return Err(Errors::VestingLengthTooLong(VestingLengthTooLong {}))
            }
        }

        Ok(())
    }

    // Ensure percentage defined is not more than 100%
    pub fn validate_percentage_unlocked(&self, vesting_length: U256, percentage_unlocked: U256) -> Result<(), Errors> {
        if vesting_length != U256::ZERO && percentage_unlocked > U256::from(ONE_HUNDRED_PERCENT) {
            return Err(Errors::InvalidPercentage(InvalidPercentage {}))
        }

        Ok(())
    }
}