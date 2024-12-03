//! Fixed-cost token sale contract that focuses on total number of tokens being sold and offers optional linear vesting of tokens (without cliff or instant unlock support)
//! If token vesting is enabled, users can tokenize the claim of tokens in an NFT allowing the owner of the NFT to have exclusivity on claiming the remaining unlocks (if applicable)
//! The program is ABI-equivalent with Solidity, which means you can call it from both Solidity and Rust. To do this, run `cargo stylus export-abi`.

// Allow `cargo stylus export-abi` to generate a main function.
#![cfg_attr(not(feature = "export-abi"), no_main)]

extern crate alloc;

use alloy_sol_types::sol; // Define errors and interfaces
use stylus_sdk::{
    alloy_primitives::{U256, Address},
    prelude::*, // Contains common traits and macros.
    block,      // Includes block::timestamp
    msg,        // Access msg::sender
    evm         // Events
};

sol_interface! {
    interface IERC20 {
        function transfer(address, uint256) external returns (bool);
        function transferFrom(address, address, uint256) external returns (bool);
    }

    interface IERC721 {
        function ownerOf(uint256) external returns (address);
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
    error NotInitialized();
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
    error NoTokensPurchased();
    error AlreadyTokenized();
    error AllTokensClaimed();
    error TokensAreVested();
    error TransferFailed();

    event TokensPurchased(address indexed user, uint256 amount);
    event TokenizedVestingEnabled(address indexed user, uint256 indexed nft_token_id);
    event TokensClaimed(address indexed user, address indexed recipient, uint256 amount);
}

/// Exporting Solidity errors defined in sol! as Rust enums
#[derive(SolidityError)]
pub enum Errors {
    OnlyOwner(OnlyOwner),
    NotInitialized(NotInitialized),
    AlreadyInitialized(AlreadyInitialized),
    ZeroValueArgumentInjected(ZeroValueArgumentInjected),
    InvalidPercentage(InvalidPercentage),
    VestingLengthTooShort(VestingLengthTooShort),
    VestingLengthTooLong(VestingLengthTooLong),
    OnlyOnePurchase(OnlyOnePurchase),
    SoldOut(SoldOut),
    VestingNotEnabled(VestingNotEnabled),
    NoTokensVested(NoTokensVested),
    NoTokensPurchased(NoTokensPurchased),
    AlreadyTokenized(AlreadyTokenized),
    AllTokensClaimed(AllTokensClaimed),
    TokensAreVested(TokensAreVested),
    TransferFailed(TransferFailed)
}

/// One day defined in seconds as the minimum vesting length if applicable
const MIN_VESTING_LENGTH: i32 = 86_400;

/// 365 days defined in seconds as the maximum vesting length if applicable
const MAX_VESTING_LENGTH: i32 = 31_536_000;

/// External methods for `TokenSaleWithTokenizedVesting`
#[public]
impl TokenSaleWithTokenizedVesting {

    /// Initialize the smart contract
    ///
    /// # Arguments
    ///
    /// * `token` - The address of the ERC20 being sold
    /// * `currency` - The address of the ERC 20 payment token
    /// * `price_per_token` - Price in the currency per token being purchased
    /// * `total_tokens_available` - Total number of tokens available for purchase
    /// * `total_vesting_length_in_seconds` - If vesting is to be enabled, specify the vesting length
    /// * `nft_claim` - Address of the ERC721 smart contract that can tokenize vesting if available
    pub fn init(
        &mut self,
        token: Address,
        currency: Address,
        price_per_token: U256,
        total_tokens_available: U256,
        total_vesting_length_in_seconds: U256,
        nft_claim: Address,
    ) -> Result<(), Errors> {
        // Perform required validation
        self.validate_initialization()?;
        self.validate_price_per_token(price_per_token)?;
        self.validate_address(token)?;
        self.validate_address(currency)?;
        self.validate_total_tokens_for_sale(total_tokens_available)?;
        self.validate_vesting_length(total_vesting_length_in_seconds)?;
        self.validate_address(nft_claim)?;

        // Setup the smart contract by configuring storage
        self.initialized.set(true);
        self.owner.set(msg::sender());
        self.token.set(token);
        self.currency.set(currency);
        self.price_per_token.set(price_per_token);
        self.total_tokens_available.set(total_tokens_available);
        self.total_vesting_length_in_seconds.set(total_vesting_length_in_seconds);
        self.nft_claim.set(nft_claim);

        Ok(())
    }

    /// Main entry point for users to buy tokens
    ///
    /// # Arguments
    ///
    /// * `amount` - Number of whole tokens being purchase which will calculate cost
    pub fn purchase_tokens(&mut self, amount: U256) -> Result<(), Errors> {
        // No need to proceed if the contract is not yet initialized
        self.validate_is_initialized()?;

        // For simplicity on vesting, we only let the address buy a token allocation once. They can create other addresses if they want more
        let tokens_purchased_by_user = self.tokens_purchased.get(msg::sender());
        if tokens_purchased_by_user > U256::ZERO {
            return Err(Errors::OnlyOnePurchase(OnlyOnePurchase {}))
        }

        // Check if global limit has been reached
        let total_tokens_purchased = self.total_tokens_purchased.get();
        let purchase_amount = amount * U256::from(1_i32.pow(18));
        if total_tokens_purchased + purchase_amount > self.total_tokens_available.get() {
            return Err(Errors::SoldOut(SoldOut {}))
        }

        // Record how many tokens user is buying and when they bought it
        self.tokens_purchased.setter(msg::sender()).set(purchase_amount);
        self.tokens_purchased_at.setter(msg::sender()).set(U256::from(block::timestamp()));
        self.total_tokens_purchased.set(total_tokens_purchased + purchase_amount);

        // calculate cost
        let cost = amount * self.price_per_token.get();
        let owner = self.owner.get();

        // Log the purchase and conclude the transaction
        evm::log(TokensPurchased {
            user: msg::sender(),
            amount
        });

        // Do the transfer
        match IERC20::new(self.currency.get()).transfer_from(
            self,
            msg::sender(), 
            owner,
            cost
        ) {
            Ok(transfer_success) => if transfer_success { 
                Ok(()) 
            } else { 
                Err(Errors::TransferFailed(TransferFailed {})) 
            },
            Err(_) => Err(Errors::TransferFailed(TransferFailed {}))
        }
    }

    /// Allows a user that purchased tokens to nominate an NFT that is allowed to claim vested tokens if applicable
    ///
    /// # Arguments
    ///
    /// * `token_id` - The token that can claim vested tokens regardless of its future owner
    pub fn enable_tokenized_vesting(&mut self, token_id: U256) -> Result<(), Errors> {
        // Validate whether it is possible to enable tokenized vesting
        let _ = self.validate_vesting_enabled()?;

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
        
        // Check they have not claimed everything
        if self.tokens_claimed.get(msg::sender()) == tokens_purchased_by_user {
            return Err(Errors::AllTokensClaimed(AllTokensClaimed {}))
        }

        // Record the NFT that tokenized the vesting so that its owner can start claiming tokens
        self.nft_claim_token_id.setter(msg::sender()).set(token_id);

        // Log the vesting being enabled and conclude the transaction
        evm::log(TokenizedVestingEnabled {
            user: msg::sender(),
            nft_token_id: token_id
        });
        
        Ok(())
    }
 
    /// Allow a user to claim vested tokens as long as it is active and not tokenized
    pub fn claim_tokens(&mut self) -> Result<(), Errors> {
        let nft_claim_token_id = self.nft_claim_token_id.get(msg::sender());
        if nft_claim_token_id != U256::ZERO {
            return Err(Errors::AlreadyTokenized(AlreadyTokenized {}))
        }

        self.claim_tokens_from_user(msg::sender(), msg::sender())
    }

    /// If tokenized vesting is enabled, then allow the owner of the NFT to claim the vested tokens
    pub fn claim_tokens_by_nft(&mut self, user: Address) -> Result<(), Errors> {
        self.validate_sender_owns_nft(self.nft_claim_token_id.get(user))?;
        self.claim_tokens_from_user(user, msg::sender())
    }

    /// When vesting is not enabled, allow the purchaser of tokens to claim all of the unlocked tokens
    pub fn claim_unlocked_tokens(&mut self) -> Result<(), Errors> {
        // This function is only for token sales that have no vesting
        if self.total_vesting_length_in_seconds.get() != U256::ZERO {
            return Err(Errors::TokensAreVested(TokensAreVested {}))
        }

        // Ensure the user has not claimed anything
        if self.tokens_claimed.get(msg::sender()) != U256::ZERO {
            return Err(Errors::AllTokensClaimed(AllTokensClaimed {}))
        }

        // Record the claim in state
        let tokens_purchased = self.tokens_purchased.get(msg::sender());
        if tokens_purchased == U256::ZERO {
            return Err(Errors::NoTokensPurchased(NoTokensPurchased {}))
        }

        self.tokens_claimed.setter(msg::sender()).set(tokens_purchased);
        self.tokens_claimed_at.setter(msg::sender()).set(U256::from(block::timestamp()));

        // Log the amount of tokens sent and conclude the transaction
        evm::log(TokensClaimed {
            user: msg::sender(),
            recipient: msg::sender(),
            amount: tokens_purchased
        });

        // Send the user all the tokens that they purchased
        match IERC20::new(self.token.get()).transfer(
            self,
            msg::sender(),
            tokens_purchased
        ) {
            Ok(transfer_success) => if transfer_success { 
                Ok(()) 
            } else { 
                Err(Errors::TransferFailed(TransferFailed {})) 
            },
            Err(_) => Err(Errors::TransferFailed(TransferFailed {}))
        }
    }

}

// Internal methods for `TokenSaleWithTokenizedVesting`
impl TokenSaleWithTokenizedVesting {
    /// Function ensuring we are initialized
    pub fn validate_is_initialized(&self) -> Result<(), Errors> {
        if !self.initialized.get() {
            return Err(Errors::NotInitialized(NotInitialized {}))
        }

        Ok(())
    }

    /// Function ensuring we are not already initialized
    pub fn validate_initialization(&self) -> Result<(), Errors> {
        if self.initialized.get() {
            return Err(Errors::AlreadyInitialized(AlreadyInitialized {}))
        } 

        Ok(())
    }

    /// Function ensuring sender is owner of the smart contract (simple ownership)
    pub fn validate_sender_is_owner(&self) -> Result<(), Errors> {
        if msg::sender() != self.owner.get() {
            return Err(Errors::OnlyOwner(OnlyOwner {}))
        }
        
        Ok(())
    }

    /// Function ensuring a zero price is not supplied to the smart contract
    pub fn validate_price_per_token(&self, price_per_token: U256) -> Result<(), Errors> {
        if price_per_token == U256::ZERO {
            return Err(Errors::ZeroValueArgumentInjected(ZeroValueArgumentInjected {}))
        }

        Ok(())
    }

    /// Function ensuring that a zero value is not supplied for an address
    pub fn validate_address(&self, value: Address) -> Result<(), Errors> {
        if value == Address::default() {
            return Err(Errors::ZeroValueArgumentInjected(ZeroValueArgumentInjected {}))
        }

        Ok(())
    }

    /// Function ensuring that total number of tokens being sold is not zero
    pub fn validate_total_tokens_for_sale(&self, total_tokens: U256) -> Result<(), Errors> {
        if total_tokens == U256::ZERO {
            return Err(Errors::ZeroValueArgumentInjected(ZeroValueArgumentInjected {}))
        }

        Ok(())
    }

    /// Function ensuring that when vesting length is not zero, it is a sensible length for users of the smart contract
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

    /// Function ensuring that we only proceed if vesting is enabled returning the vesting length in seconds
    pub fn validate_vesting_enabled(&self) -> Result<U256, Errors> {
        let total_vesting_length_in_seconds = self.total_vesting_length_in_seconds.get();
        if total_vesting_length_in_seconds == U256::ZERO {
            return Err(Errors::VestingNotEnabled(VestingNotEnabled {}))
        }

        Ok(total_vesting_length_in_seconds)
    }

    /// Function ensuring msg.sender is the owner of a ERC721 token
    pub fn validate_sender_owns_nft(&mut self, token_id: U256) -> Result<(), Errors> {
        let owner = match IERC721::new(self.nft_claim.get()).owner_of(self, token_id) {
            Ok(owner) => owner,
            Err(_) => Address::default()
        };

        if owner != msg::sender() {
            return Err(Errors::OnlyOwner(OnlyOwner {}))
        }

        Ok(())
    }

    /// Logic for performing a claim of tokens if the tokens are vested, releasing a tranche since the last timestamp
    ///
    /// # Arguments
    ///
    /// * `user` - The Ethereum wallet address of the user that purchased tokens
    /// * `recipient` - The Ethereum wallet address which will receive unlocked tokens which can be different from the user
    pub fn claim_tokens_from_user(
        &mut self, 
        user: Address, 
        recipient: Address
    ) -> Result<(), Errors> {
        // Check whether tokens are vested by anyone purchasing
        let total_vesting_length_in_seconds = self.validate_vesting_enabled()?;

        // Check whether the user purchased any tokens
        let tokens_purchased_by_user = self.tokens_purchased.get(user);
        if tokens_purchased_by_user == U256::ZERO {
            return Err(Errors::NoTokensVested(NoTokensVested {}))
        }

        // Check when the last claim happened
        let tokens_purchased_at = self.tokens_purchased_at.get(user);
        let tokens_claimed_by_user = self.tokens_claimed.get(user);
        let mut last_user_claim_timestamp = self.tokens_claimed_at.get(user);
        if tokens_claimed_by_user == U256::ZERO {
            last_user_claim_timestamp = tokens_purchased_at;
        }

        // Check they have not claimed everything
        if tokens_claimed_by_user == tokens_purchased_by_user {
            return Err(Errors::AllTokensClaimed(AllTokensClaimed {}))
        }

        // Calculate how many tokens to release 
        let current_time = U256::from(block::timestamp());
        let last_token_claim_at = tokens_purchased_at + total_vesting_length_in_seconds;
        let mut tokens_claimed_setter = self.tokens_claimed.setter(user);
        let mut tokens_claimed_at_setter = self.tokens_claimed_at.setter(user);
        let amount: U256 = if current_time >= last_token_claim_at {
            // Update the claim amount and last claim timestamp which is upperbound to the end
            tokens_claimed_setter.set(tokens_purchased_by_user);
            tokens_claimed_at_setter.set(last_token_claim_at);

            // Amount to transfer will be all remaining tokens
            tokens_purchased_by_user - tokens_claimed_by_user
        } else {
            // Amount to transfer will be based on how many have unlocked since the last claim
            let time_since_last_claim = current_time - last_user_claim_timestamp;
            let tokens_per_second_to_claim = ((tokens_purchased_by_user * U256::from(1e12)) / total_vesting_length_in_seconds) / U256::from(1e12);
            let transfer_amount: U256 = time_since_last_claim * tokens_per_second_to_claim;
            
            // Update the total claimed by the user and the current timestamp
            tokens_claimed_setter.set(tokens_claimed_by_user + transfer_amount);
            tokens_claimed_at_setter.set(current_time);

            transfer_amount
        };

        // Log the amount of tokens received and distinguish between who paid and who is receiving the tokens
        evm::log(TokensClaimed {
            user,
            recipient,
            amount
        });

        // Transfer the unlocked tokens to the target recipient
        match IERC20::new(self.token.get()).transfer(
            self,
            recipient,
            amount
        ) {
            Ok(transfer_success) => if transfer_success { 
                Ok(()) 
            } else { 
                Err(Errors::TransferFailed(TransferFailed {})) 
            },
            Err(_) => Err(Errors::TransferFailed(TransferFailed {}))
        }
    }
}