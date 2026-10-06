// DESTINATION (where this file goes): kots-contracts/shared/kots-common/src/errors.rs
// kots-contracts/shared/kots-common/src/errors.rs
//! KOTS Error Codes
//! 
//! Complete error code system preserved from the original contract.
//! All error codes maintained for compatibility.

use anchor_lang::prelude::*;

#[error_code]
pub enum KotsError {
    // Rate limiting and user management (1000-1099)
    #[msg("Rate limit exceeded")]
    RateLimitExceeded = 1000,
    #[msg("Contest creation limit reached")]
    ContestCreationLimit = 1001,
    #[msg("Interaction limit reached")]
    InteractionLimit = 1002,
    #[msg("Interaction attempted too soon")]
    InteractionTooSoon = 1003,
    #[msg("User is suspended")]
    UserSuspended = 1004,
    #[msg("User account not found")]
    UserAccountNotFound = 1005,
    #[msg("Excessive transfer amount for treasure hunt")]
    ExcessiveTransferAmount = 1006,
    #[msg("Daily transfer limit exceeded")]
    DailyTransferLimitExceeded = 1007,
    #[msg("Lifetime distribution cap exceeded")]
    LifetimeDistributionCap = 1008,
    
    // Contest management (2000-2099)
    #[msg("Contest not found")]
    ContestNotFound = 2000,
    #[msg("Contest already finalized")]
    ContestAlreadyFinalized = 2001,
    #[msg("Contest not finalized")]
    ContestNotFinalized = 2002,
    #[msg("Contest not ended")]
    ContestNotEnded = 2003,
    #[msg("Invalid contest duration")]
    InvalidContestDuration = 2004,
    #[msg("Contest is full")]
    ContestFull = 2005,
    #[msg("Duplicate meme ID")]
    DuplicateMemeId = 2006,
    #[msg("Invalid meme ID")]
    InvalidMemeId = 2007,
    #[msg("Not a contest participant")]
    NotContestParticipant = 2008,
    #[msg("Meme timestamp outside acceptable window")]
    InvalidMemeTimestamp = 2017,
    #[msg("Meme account already exists")]
    MemeAlreadyExists = 2019,
    #[msg("Invalid PDA address")]
    InvalidPDA = 2024,
    #[msg("Contest is paused")]
    ContestPaused = 2009,
    #[msg("Insufficient participants")]
    InsufficientParticipants = 2010,
    #[msg("Contest not active")]
    ContestNotActive = 2011,
    #[msg("Already participating in contest")]
    AlreadyParticipating = 2012,
    #[msg("Contest not paused")]
    ContestNotPaused = 2013,
    #[msg("Invalid CPI program")]
    InvalidCpiProgram = 2014,
    #[msg("CPI call failed")]
    CpiCallFailed = 2015,
    #[msg("Duplicate entry found")]
    DuplicateEntry = 2016,
    
    // New v3.1 error codes (2020-2029)
    #[msg("Contest has ended")]
    ContestEnded = 2020,
    #[msg("Duplicate vote for same hieroglyph")]
    DuplicateVoteSameHieroglyph = 2021,
    #[msg("Insufficient stake amount")]
    InsufficientStake = 2022,
    #[msg("Sybil attack detected")]
    SybilAttackDetected = 2023,
    
    // Prize and finalization (3000-3099)
    #[msg("Unauthorized finalizer")]
    UnauthorizedFinalizer = 3000,
    #[msg("Prize not found")]
    PrizeNotFound = 3001,
    #[msg("Prize already claimed")]
    PrizeAlreadyClaimed = 3002,
    #[msg("No prize amount")]
    NoPrizeAmount = 3003,
    #[msg("Invalid winner record")]
    InvalidWinnerRecord = 3007,
    #[msg("Unauthorized claim attempt")]
    UnauthorizedClaim = 3008,
    #[msg("Jackpot not initialized")]
    JackpotNotInitialized = 3004,
    #[msg("No jackpot amount")]
    NoJackpotAmount = 3005,
    #[msg("Insufficient vault funds")]
    InsufficientVaultFunds = 3006,
    #[msg("Distribution failed")]
    DistributionFailed = 3009,
    #[msg("No winners found")]
    NoWinnersFound = 3012,
    #[msg("Auto distribution not ready")]
    AutoDistributionNotReady = 3010,
    #[msg("Manual claim window expired")]
    ManualClaimWindowExpired = 3011,
    
    // Ownership and market validation (4000-4099)
    #[msg("Ownership limit exceeded")]
    OwnershipLimitExceeded = 4000,
    #[msg("Market cap validation failed")]
    MarketCapValidationFailed = 4001,
    
    // Emergency and security (5000-5099)
    #[msg("Emergency pause is active")]
    EmergencyPauseActive = 5000,
    #[msg("Unauthorized emergency operation")]
    UnauthorizedEmergencyOperation = 5001,
    #[msg("Emergency not active")]
    EmergencyNotActive = 5002,
    
    // Blacklist management (6000-6099)
    #[msg("Wallet is blacklisted")]
    BlacklistedWallet = 6000,
    #[msg("Unauthorized blacklist operation")]
    UnauthorizedBlacklistOperation = 6001,
    #[msg("Cannot blacklist system wallet")]
    CannotBlacklistSystemWallet = 6002,
    #[msg("Wallet not blacklisted")]
    WalletNotBlacklisted = 6003,
    #[msg("Fee-distribution destination is not the canonical wallet for this leg")]
    OffCanonicalDistributionWallet = 6004,
    
    // Validation errors (7000-7099)
    #[msg("Invalid amount")]
    InvalidAmount = 7000,
    #[msg("Amount too large")]
    AmountTooLarge = 7001,
    #[msg("Invalid percentage")]
    InvalidPercentage = 7002,
    #[msg("Invalid basis points")]
    InvalidBasisPoints = 7003,
    #[msg("Invalid wallet address")]
    InvalidWallet = 7004,
    #[msg("Arithmetic underflow")]
    ArithmeticUnderflow = 7005,
    #[msg("String too long")]
    StringTooLong = 7006,
    #[msg("Division by zero")]
    DivisionByZero = 7007,
    #[msg("Invalid duration")]
    InvalidDuration = 7008,
    #[msg("Duration too long")]
    DurationTooLong = 7009,
    #[msg("Invalid hieroglyph ID")]
    InvalidHieroglyphID = 7010,
    #[msg("Invalid parameter value")]
    InvalidParameter = 7011,
    
    // Gas oracle (7100-7199)
    #[msg("Unauthorized gas oracle update")]
    UnauthorizedGasOracleUpdate = 7100,
    #[msg("Gas price change too large")]
    GasPriceChangeTooLarge = 7101,
    #[msg("Invalid gas price bounds")]
    InvalidGasPriceBounds = 7102,
    #[msg("Gas oracle not initialized")]
    GasOracleNotInitialized = 7103,
    #[msg("Update too frequent")]
    UpdateTooFrequent = 7104,
    #[msg("Daily change limit exceeded")]
    DailyChangeLimitExceeded = 7105,
    #[msg("Gas oracle consensus failed")]
    GasOracleConsensusFailed = 7110,
    #[msg("Unauthorized gas oracle")]
    UnauthorizedGasOracle = 7111,
    
    // Token and financial (8000-8099)
    #[msg("Insufficient funds")]
    InsufficientFunds = 8000,
    #[msg("Invalid token account")]
    InvalidTokenAccount = 8001,
    #[msg("Token transfer failed")]
    TokenTransferFailed = 8002,
    #[msg("Invalid token amount")]
    InvalidTokenAmount = 8003,
    #[msg("Invalid token mint")]
    InvalidTokenMint = 8004,
    #[msg("Token account not owned")]
    TokenAccountNotOwned = 8005,
    #[msg("Token account frozen")]
    TokenAccountFrozen = 8006,
    #[msg("Token account closed")]
    TokenAccountClosed = 8007,
    
    // Account and initialization (9000-9099)
    #[msg("Wallet not configured")]
    WalletNotConfigured = 9000,
    #[msg("Invalid wallet format")]
    InvalidWalletFormat = 9001,
    #[msg("Initialization failed")]
    InitializationFailed = 9002,
    #[msg("Account already initialized")]
    AccountAlreadyInitialized = 9003,
    #[msg("Invalid PDA derivation")]
    InvalidPdaDerivation = 9004,
    #[msg("Uninitialized account")]
    UninitializedAccount = 9005,
    #[msg("Invalid account owner")]
    InvalidAccountOwner = 9006,
    #[msg("Account data too large")]
    AccountDataTooLarge = 9007,
    #[msg("Account data corrupted")]
    AccountDataCorrupted = 9008,
    #[msg("Account not writable")]
    AccountNotWritable = 9009,
    #[msg("Account not executable")]
    AccountNotExecutable = 9010,
    #[msg("Account rent not exempt")]
    AccountRentNotExempt = 9011,
    
    // Arithmetic and validation (10000-10099)
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow = 10000,
    #[msg("Serialization error")]
    SerializationError = 10001,
    
    // File and data operations (10100-10199)
    #[msg("Invalid file format")]
    InvalidFileFormat = 10100,
    #[msg("File size too large")]
    FileSizeTooLarge = 10101,
    #[msg("Invalid IPFS hash")]
    InvalidIpfsHash = 10102,
    #[msg("Data verification failed")]
    DataVerificationFailed = 10103,
    #[msg("Invalid metadata")]
    InvalidMetadata = 10104,
    #[msg("Metadata too large")]
    MetadataTooLarge = 10105,
    #[msg("Invalid content type")]
    InvalidContentType = 10106,
    #[msg("Upload failed")]
    UploadFailed = 10107,
    #[msg("Invalid contest label")]
    InvalidContestLabel = 10108,
    #[msg("Invalid IPFS hash format")]
    InvalidIPFSHash = 10109,
    #[msg("Insufficient payment amount")]
    InsufficientPayment = 10110,
    #[msg("Invalid payment receipt")]
    InvalidPaymentReceipt = 10111,
    #[msg("Unauthorized access")]
    UnauthorizedAccess = 10112,
    
    // Governance and upgrade (10200-10299)
    #[msg("Invalid proposal")]
    InvalidProposal = 10200,
    #[msg("Proposal not active")]
    ProposalNotActive = 10201,
    #[msg("Voting period expired")]
    VotingPeriodExpired = 10202,
    #[msg("Invalid proposal status")]
    InvalidProposalStatus = 10203,
    #[msg("Insufficient permissions")]
    InsufficientPermissions = 10204,
    #[msg("Unauthorized upgrade")]
    UnauthorizedUpgrade = 10205,
    #[msg("Upgrade failed")]
    UpgradeFailed = 10206,
    #[msg("Invalid version")]
    InvalidVersion = 10207,
    #[msg("Version mismatch")]
    VersionMismatch = 10208,
    #[msg("Upgrade in progress")]
    UpgradeInProgress = 10209,
    #[msg("Governance not active")]
    GovernanceNotActive = 10210,
    #[msg("Invalid vote")]
    InvalidVote = 10211,
    #[msg("Already voted")]
    AlreadyVoted = 10212,
    #[msg("Quorum not met")]
    QuorumNotMet = 10213,
    #[msg("Invalid voting weight")]
    InvalidVotingWeight = 10214,
    #[msg("Stake requirement not met")]
    StakeRequirementNotMet = 10215,
    
    // Additional errors for contest management
    #[msg("Serialization failed")]
    SerializationFailed = 10216,
    #[msg("Invalid program ID")]
    InvalidProgramId = 10217,
    #[msg("Contest duration too short")]
    ContestDurationTooShort = 10218,
    #[msg("Contest duration too long")]
    ContestDurationTooLong = 10219,
    #[msg("Minimum participants too low")]
    MinParticipantsTooLow = 10220,
    #[msg("Entry fee too low")]
    EntryFeeTooLow = 10221,
    #[msg("Entry fee too high")]
    EntryFeeTooHigh = 10222,
    #[msg("Invalid upgrade version")]
    InvalidUpgradeVersion = 10223,
    #[msg("Execution delay not met")]
    ExecutionDelayNotMet = 10224,
    #[msg("No upgrade in progress")]
    NoUpgradeInProgress = 10225,
    #[msg("Suspicious activity detected")]
    SuspiciousActivity = 10226,
    #[msg("Scoring already complete")]
    ScoringAlreadyComplete = 10227,
    #[msg("Scoring not complete")]
    ScoringNotComplete = 10228,
    #[msg("Missing required signature")]
    MissingRequiredSignature = 10229,
    #[msg("Invalid winner position")]
    InvalidWinnerPosition = 10230,
    #[msg("Winner position not found")]
    WinnerPositionNotFound = 10231,
    #[msg("Invalid participant")]
    InvalidParticipant = 10232,
    #[msg("Fraud detected")]
    FraudDetected = 10233,
    #[msg("Invalid market cap")]
    InvalidMarketCap = 10234,
    #[msg("Stale oracle data")]
    StaleOracleData = 10235,
    #[msg("Auto distribution disabled")]
    AutoDistributionDisabled = 10236,
    #[msg("Auto distribution too early")]
    AutoDistributionTooEarly = 10237,
    #[msg("Auto distribution already executed")]
    AutoDistributionAlreadyExecuted = 10238,
    #[msg("Invalid distribution percentage")]
    InvalidDistributionPercentage = 10239,
    #[msg("Transaction amount too large")]
    TransactionTooLarge = 10240,
    #[msg("Upgrade incomplete")]
    UpgradeIncomplete = 10241,
    #[msg("Cooldown period is active")]
    CooldownActive = 10242,
    #[msg("Low behavior score detected")]
    LowBehaviorScore = 10243,
    
    #[msg("Insufficient treasury funds")]
    InsufficientTreasuryFunds = 10244,
    
    #[msg("Unauthorized caller program")]
    UnauthorizedCaller = 10245,
    
    #[msg("Account owned by wrong program")]
    AccountOwnedByWrongProgram = 10246,
    
    #[msg("Instruction disabled")]
    InstructionDisabled = 10247,
    
    #[msg("Treasury accounts not properly initialized")]
    TreasuryNotInitialized = 10248,

    // APPENDED 2026-07-29 (O41). Explicit discriminants throughout, so appending
    // renumbers nothing.
    #[msg("Blacklist is full — no free slot; remove an entry before adding")]
    BlacklistFull = 10249,

    // APPENDED 2026-08 (ACT-21 — on-chain treasury timelock + veto). Explicit
    // discriminants, so appending renumbers nothing.
    #[msg("A queued transfer must carry a non-empty purpose")]
    PurposeRequired = 10250,
    #[msg("Purpose exceeds the maximum length")]
    PurposeTooLong = 10251,
    #[msg("This queued transfer has been vetoed by an owner")]
    TransferVetoed = 10252,
    #[msg("This queued transfer has already settled")]
    TransferAlreadySettled = 10253,
    #[msg("The timelock window has not yet elapsed for this transfer")]
    TimelockNotElapsed = 10254,
    #[msg("This transfer would exceed the rolling-window outflow ceiling")]
    OutflowCeilingExceeded = 10255,
    #[msg("No registered watcher has attested liveness within the freshness window — external settle halted")]
    WatcherLivenessStale = 10256,
    #[msg("Signer is not a registered watcher")]
    WatcherNotRegistered = 10257,
    #[msg("This watcher key is already registered")]
    WatcherAlreadyRegistered = 10258,
    #[msg("The watcher registry is full")]
    WatcherRegistryFull = 10259,
    #[msg("Purpose is not a structured accounting key (contest-award:N / contest-voter-share:N / jackpot / treasury-op:N / tier-stake:N)")]
    PurposeKeyInvalid = 10260,
    #[msg("Purpose key does not match the targeted bucket")]
    PurposeKeyMismatch = 10261,
    #[msg("This distribution would over-draw the bucket's entitled budget")]
    BucketOverdraw = 10262,
    #[msg("Signer is not the configured distribution authority")]
    UnauthorizedDistributor = 10263,
    #[msg("This jackpot draw would exceed the on-chain jackpot pool balance")]
    JackpotPoolExceeded = 10264,
    #[msg("The pinned jackpot pool account is absent, wrong, or unreadable")]
    JackpotPoolUnreadable = 10265,
    #[msg("Signer is not the pinned bucket-credit authority")]
    UnauthorizedBucketCreditor = 10266,
    #[msg("Signer is not the configured routine operational authority")]
    UnauthorizedRoutineOperator = 10267,
    #[msg("Destination is not on the routine whitelist")]
    RoutineDestinationNotWhitelisted = 10268,
    #[msg("This transfer exceeds the routine per-transfer cap")]
    RoutinePerTransferCapExceeded = 10269,
    #[msg("This transfer would exceed the routine daily aggregate cap")]
    RoutineDailyCapExceeded = 10270,
    #[msg("The routine whitelist is full")]
    RoutineWhitelistFull = 10271,
    #[msg("Destination is already on the routine whitelist")]
    RoutineDestinationAlreadyListed = 10272,
    #[msg("Destination is not on the routine whitelist to remove")]
    RoutineDestinationNotFound = 10273,
    // O32 HEAP GATE (2026-08-04): the monolithic distributions are refused at
    // the door — every hooked-mint leg's resolver copy is charged against one
    // 32 KB Anchor heap, and the monoliths carry too many legs to complete.
    #[msg("Monolithic distribution is heap-bound on hooked-mint legs; use the split Part instructions (max two legs each)")]
    MonolithicDistributionHeapBound = 10274,
}