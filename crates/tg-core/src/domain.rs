//! Port of `main/main_domain.h`, `main/main_domain.cpp`, and non-network parts of
//! `main/main_account.h`.
//!
//! Manages multiple Telegram accounts in a single `Domain`, tracking account indices,
//! the active account, the dynamic account limit (`3` base, up to `6` with Telegram Premium
//! or test-mode accounts), redundant session-less account cleanup, and persistence hooks
//! abstracted behind [`DomainStorage`] (implemented by `tg-storage`).

use std::collections::BTreeMap;

use thiserror::Error;

/// Default maximum accounts without Telegram Premium (`Main::Domain::kMaxAccounts`).
pub const MAX_ACCOUNTS: usize = 3;

/// Hard maximum accounts with Telegram Premium (`Main::Domain::kPremiumMaxAccounts`).
pub const PREMIUM_MAX_ACCOUNTS: usize = 6;

/// Telegram server environment (`MTP::Environment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Environment {
    /// Production Telegram servers.
    #[default]
    Production,
    /// Telegram test DC servers.
    Test,
}

/// Startup result returned by [`Domain::start`] (`Storage::StartResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartResult {
    /// Domain loaded and started successfully.
    Success,
    /// Provided local passcode was incorrect.
    IncorrectPasscode,
    /// Passcode was correct or absent, but stored data was corrupt/reset.
    IncorrectPasscodeLegacy,
}

/// Errors returned by [`DomainStorage`] implementations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainStorageError {
    /// Incorrect local passcode prevented decrypting `key_datas`.
    #[error("incorrect local passcode")]
    IncorrectPasscode,
    /// Persistence backend I/O or format error.
    #[error("storage error: {0}")]
    Backend(String),
}

/// Errors returned by [`Domain`] account management operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    /// Domain has not been started yet.
    #[error("domain has not been started")]
    NotStarted,
    /// Domain is already started.
    #[error("domain is already started")]
    AlreadyStarted,
    /// Account limit (`PREMIUM_MAX_ACCOUNTS` or current `max_accounts()`) reached.
    #[error("maximum account limit ({limit}) reached")]
    AccountLimitReached {
        /// Active maximum account limit.
        limit: usize,
    },
    /// Attempted to add an account with an index that already exists.
    #[error("duplicate account index: {0}")]
    DuplicateAccountIndex(i32),
    /// Requested account index does not exist in the domain.
    #[error("account index not found: {0}")]
    AccountNotFound(i32),
    /// Cannot remove the last remaining account without resetting the domain.
    #[error("cannot remove the only account in the domain")]
    CannotRemoveLastAccount,
    /// Underlying persistence error.
    #[error("storage error: {0}")]
    Storage(#[from] DomainStorageError),
}

/// Non-network session metadata attached to an authenticated [`Account`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSession {
    /// Telegram user ID.
    pub user_id: i64,
    /// Stable unique session ID used for `CoreSettings::accounts_order`.
    pub unique_id: u64,
    /// Whether this user has an active Telegram Premium subscription.
    pub premium: bool,
    /// Whether this session runs against Telegram's test DCs.
    pub is_test_mode: bool,
    /// Current username (used for crash report annotation).
    pub username: String,
    /// Unread badge count contributed by this account.
    pub unread_badge: i32,
    /// Whether all unread messages in this account are muted.
    pub unread_badge_muted: bool,
}

/// Non-network representation of one account slot (`Main::Account`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Unique slot index inside the domain (`0`, `1`, ...).
    pub index: i32,
    /// Profile data file prefix (e.g. `"data"` or `-key` value).
    pub data_name: String,
    /// Target Telegram environment.
    pub environment: Environment,
    /// Main DC ID for this account (`0` if not yet set).
    pub main_dc_id: i32,
    /// Authenticated session metadata, if signed in.
    pub session: Option<AccountSession>,
    /// Whether logout is currently in progress.
    pub logging_out: bool,
    /// Whether session teardown is currently running (re-entrancy guard).
    pub destroying_session: bool,
    /// Whether a separate OS window is currently open for this account.
    pub has_separate_window: bool,
}

impl Account {
    /// Creates a new unauthenticated account slot.
    #[must_use]
    pub fn new(index: i32, data_name: impl Into<String>, environment: Environment) -> Self {
        Self {
            index,
            data_name: data_name.into(),
            environment,
            main_dc_id: 0,
            session: None,
            logging_out: false,
            destroying_session: false,
            has_separate_window: false,
        }
    }

    /// Returns `true` if this account currently has an authenticated session.
    #[must_use]
    pub fn session_exists(&self) -> bool {
        self.session.is_some()
    }

    /// Returns `true` if this account contributes +1 to the dynamic account cap
    /// (`session.premium || session.is_test_mode` in `main_domain.cpp:617-619`).
    #[must_use]
    pub fn expands_account_limit(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.premium || s.is_test_mode)
    }

    /// Returns the session's `unique_id`, if authenticated.
    #[must_use]
    pub fn session_unique_id(&self) -> Option<u64> {
        self.session.as_ref().map(|s| s.unique_id)
    }

    /// Attaches an authenticated session to this account.
    pub fn create_session(&mut self, session: AccountSession) {
        self.logging_out = false;
        self.session = Some(session);
    }

    /// Clears the authenticated session (simulating logout completion).
    pub fn destroy_session(&mut self) {
        self.destroying_session = true;
        self.session = None;
        self.logging_out = false;
        self.destroying_session = false;
    }

    /// Marks the account as logging out and clears its session.
    pub fn log_out(&mut self) {
        self.logging_out = true;
        self.destroy_session();
    }
}

/// Snapshot returned by [`DomainStorage::load_accounts`] when starting a [`Domain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDomainState {
    /// Persisted accounts loaded from storage (must be non-empty on success).
    pub accounts: Vec<Account>,
    /// Stored active account index.
    pub active_index: i32,
}

/// Persistence interface for [`Domain`] (`Storage::Domain` contract; implemented later by `tg-storage`).
pub trait DomainStorage {
    /// Loads saved accounts using `passcode` (empty slice when no passcode is set).
    fn load_accounts(&mut self, passcode: &[u8]) -> Result<LoadedDomainState, DomainStorageError>;

    /// Persists the list of account indices and currently active account index.
    fn write_accounts(
        &mut self,
        active_index: i32,
        account_indices: &[i32],
    ) -> Result<(), DomainStorageError>;

    /// Initializes storage for a newly added account slot.
    fn start_added(
        &mut self,
        index: i32,
        environment: Environment,
    ) -> Result<(), DomainStorageError>;

    /// Removes storage metadata for a removed account slot.
    fn account_removed(&mut self, index: i32) -> Result<(), DomainStorageError>;

    /// Returns `true` if a local passcode is currently configured.
    fn has_passcode(&self) -> bool;

    /// Verifies `passcode` against the configured local passcode.
    fn check_passcode(&self, passcode: &[u8]) -> bool;

    /// Clears the local passcode after the last account logs out.
    fn clear_passcode_after_reset(&mut self) -> Result<(), DomainStorageError>;

    /// Wipes existing encrypted state and starts fresh with a single empty production account.
    fn start_from_scratch(&mut self) -> Result<LoadedDomainState, DomainStorageError>;
}

/// In-memory [`DomainStorage`] implementation for unit tests and headless tooling.
#[derive(Debug, Clone)]
pub struct MemoryDomainStorage {
    data_name: String,
    passcode: Option<Vec<u8>>,
    accounts: BTreeMap<i32, Account>,
    active_index: i32,
    write_count: usize,
}

impl MemoryDomainStorage {
    /// Creates an empty in-memory storage backend.
    #[must_use]
    pub fn new(data_name: impl Into<String>) -> Self {
        Self {
            data_name: data_name.into(),
            passcode: None,
            accounts: BTreeMap::new(),
            active_index: 0,
            write_count: 0,
        }
    }

    /// Configures a local passcode for testing passcode flows.
    pub fn set_passcode(&mut self, passcode: Option<Vec<u8>>) {
        self.passcode = passcode;
    }

    /// Pre-populates an account before `Domain::start()`.
    pub fn seed_account(&mut self, account: Account) {
        self.accounts.insert(account.index, account);
    }

    /// Sets the stored active account index before `Domain::start()`.
    pub fn set_stored_active_index(&mut self, index: i32) {
        self.active_index = index;
    }

    /// Returns how many times `write_accounts()` was invoked.
    #[must_use]
    pub fn write_count(&self) -> usize {
        self.write_count
    }

    /// Returns the last persisted active account index.
    #[must_use]
    pub fn stored_active_index(&self) -> i32 {
        self.active_index
    }
}

impl DomainStorage for MemoryDomainStorage {
    fn load_accounts(&mut self, passcode: &[u8]) -> Result<LoadedDomainState, DomainStorageError> {
        if let Some(expected) = &self.passcode
            && expected.as_slice() != passcode
        {
            return Err(DomainStorageError::IncorrectPasscode);
        }
        if self.accounts.is_empty() {
            let initial = Account::new(0, &self.data_name, Environment::Production);
            self.accounts.insert(0, initial);
            self.active_index = 0;
        }
        Ok(LoadedDomainState {
            accounts: self.accounts.values().cloned().collect(),
            active_index: self.active_index,
        })
    }

    fn write_accounts(
        &mut self,
        active_index: i32,
        account_indices: &[i32],
    ) -> Result<(), DomainStorageError> {
        self.active_index = active_index;
        self.accounts.retain(|idx, _| account_indices.contains(idx));
        self.write_count += 1;
        Ok(())
    }

    fn start_added(
        &mut self,
        index: i32,
        environment: Environment,
    ) -> Result<(), DomainStorageError> {
        self.accounts
            .entry(index)
            .or_insert_with(|| Account::new(index, &self.data_name, environment));
        Ok(())
    }

    fn account_removed(&mut self, index: i32) -> Result<(), DomainStorageError> {
        self.accounts.remove(&index);
        Ok(())
    }

    fn has_passcode(&self) -> bool {
        self.passcode.is_some()
    }

    fn check_passcode(&self, passcode: &[u8]) -> bool {
        self.passcode
            .as_ref()
            .is_none_or(|expected| expected.as_slice() == passcode)
    }

    fn clear_passcode_after_reset(&mut self) -> Result<(), DomainStorageError> {
        self.passcode = None;
        Ok(())
    }

    fn start_from_scratch(&mut self) -> Result<LoadedDomainState, DomainStorageError> {
        self.passcode = None;
        self.accounts.clear();
        let initial = Account::new(0, &self.data_name, Environment::Production);
        self.accounts.insert(0, initial.clone());
        self.active_index = 0;
        Ok(LoadedDomainState {
            accounts: vec![initial],
            active_index: 0,
        })
    }
}

/// Multi-account domain (`Main::Domain` in `main/main_domain.h` / `.cpp`).
#[derive(Debug)]
pub struct Domain<S: DomainStorage> {
    data_name: String,
    storage: S,
    accounts: Vec<Account>,
    active_index: Option<i32>,
    last_active_index: Option<i32>,
}

impl<S: DomainStorage> Domain<S> {
    /// Creates a new unstarted domain backed by `storage`.
    #[must_use]
    pub fn new(data_name: impl Into<String>, storage: S) -> Self {
        Self {
            data_name: data_name.into(),
            storage,
            accounts: Vec::new(),
            active_index: None,
            last_active_index: None,
        }
    }

    /// Borrows the underlying storage implementation.
    #[must_use]
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Mutably borrows the underlying storage implementation.
    pub fn storage_mut(&mut self) -> &mut S {
        &mut self.storage
    }

    /// Returns `true` if the domain has been started and holds at least one account.
    #[must_use]
    pub fn started(&self) -> bool {
        !self.accounts.is_empty()
    }

    /// Loads accounts from storage with `passcode` and activates the stored account (`Domain::start`).
    pub fn start(&mut self, passcode: &[u8]) -> Result<StartResult, DomainError> {
        if self.started() {
            return Err(DomainError::AlreadyStarted);
        }
        match self.storage.load_accounts(passcode) {
            Ok(loaded) => {
                self.apply_loaded_state(loaded)?;
                Ok(StartResult::Success)
            }
            Err(DomainStorageError::IncorrectPasscode) => Ok(StartResult::IncorrectPasscode),
            Err(other) => Err(DomainError::Storage(other)),
        }
    }

    /// Resets the domain when the user forgets their local passcode (`Domain::resetWithForgottenPasscode`).
    pub fn reset_with_forgotten_passcode(&mut self) -> Result<(), DomainError> {
        if self.accounts.is_empty() {
            let loaded = self.storage.start_from_scratch()?;
            self.apply_loaded_state(loaded)?;
        } else {
            for account in &mut self.accounts {
                account.log_out();
            }
            let _ = self.remove_redundant_accounts()?;
        }
        Ok(())
    }

    /// Shuts down the domain and clears in-memory account state (`Domain::finish`).
    pub fn finish(&mut self) {
        self.active_index = None;
        self.last_active_index = None;
        self.accounts.clear();
    }

    fn apply_loaded_state(&mut self, loaded: LoadedDomainState) -> Result<(), DomainError> {
        self.accounts.clear();
        for account in loaded.accounts {
            if self.accounts.iter().any(|a| a.index == account.index) {
                return Err(DomainError::DuplicateAccountIndex(account.index));
            }
            self.accounts.push(account);
        }
        if self.accounts.is_empty() {
            return Err(DomainError::NotStarted);
        }
        let target = if self.accounts.iter().any(|a| a.index == loaded.active_index) {
            loaded.active_index
        } else {
            self.accounts[0].index
        };
        self.active_index = Some(target);
        let _ = self.remove_passcode_if_empty()?;
        Ok(())
    }

    /// Computes the dynamic maximum account count (`Main::Domain::maxAccounts()`):
    /// `min(premium_or_test_authed_count + 3, 6)`.
    #[must_use]
    pub fn max_accounts(&self) -> usize {
        let bonus = self
            .accounts
            .iter()
            .filter(|a| a.expands_account_limit())
            .count();
        (bonus + MAX_ACCOUNTS).min(PREMIUM_MAX_ACCOUNTS)
    }

    /// Returns all accounts in slot-registration order.
    #[must_use]
    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    /// Returns accounts sorted stably according to `accounts_order` (matching `Domain::orderedAccounts`).
    #[must_use]
    pub fn ordered_accounts(&self, accounts_order: &[u64]) -> Vec<&Account> {
        let mut refs: Vec<&Account> = self.accounts.iter().collect();
        refs.sort_by_key(|a| {
            a.session_unique_id()
                .and_then(|uid| accounts_order.iter().position(|&id| id == uid))
                .unwrap_or(usize::MAX)
        });
        refs
    }

    /// Looks up an account by its index.
    #[must_use]
    pub fn account(&self, index: i32) -> Option<&Account> {
        self.accounts.iter().find(|a| a.index == index)
    }

    /// Mutably looks up an account by its index.
    pub fn account_mut(&mut self, index: i32) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|a| a.index == index)
    }

    /// Returns the index of the active account, if started.
    #[must_use]
    pub fn active_index(&self) -> Option<i32> {
        self.active_index
    }

    /// Returns the currently active account, if started.
    #[must_use]
    pub fn active(&self) -> Option<&Account> {
        let idx = self.active_index?;
        self.account(idx)
    }

    /// Returns a mutable reference to the currently active account, if started.
    pub fn active_mut(&mut self) -> Option<&mut Account> {
        let idx = self.active_index?;
        self.account_mut(idx)
    }

    /// Returns the number of authenticated accounts (`Domain::accountsAuthedCount`).
    #[must_use]
    pub fn accounts_authed_count(&self) -> usize {
        self.accounts.iter().filter(|a| a.session_exists()).count()
    }

    /// Returns the previously active authenticated account, or the first authenticated account
    /// (`Domain::maybeLastOrSomeAuthedAccount`).
    #[must_use]
    pub fn maybe_last_or_some_authed_account(&self) -> Option<&Account> {
        let mut fallback = None;
        for account in &self.accounts {
            if !account.session_exists() {
                continue;
            }
            if Some(account.index) == self.last_active_index {
                return Some(account);
            }
            if fallback.is_none() {
                fallback = Some(account);
            }
        }
        fallback
    }

    /// Computes total unread badge count across all authenticated accounts (`Domain::updateUnreadBadge`).
    #[must_use]
    pub fn unread_badge(&self) -> (i32, bool) {
        let mut total = 0i32;
        let mut all_muted = true;
        for account in &self.accounts {
            if let Some(session) = &account.session {
                total = total.saturating_add(session.unread_badge);
                if !session.unread_badge_muted {
                    all_muted = false;
                }
            }
        }
        (total, all_muted)
    }

    /// Adds a new unauthenticated account slot using the smallest unused non-negative index
    /// (`Domain::add`).
    pub fn add(&mut self, environment: Environment) -> Result<i32, DomainError> {
        if !self.started() {
            return Err(DomainError::NotStarted);
        }
        if self.accounts.len() >= PREMIUM_MAX_ACCOUNTS {
            return Err(DomainError::AccountLimitReached {
                limit: PREMIUM_MAX_ACCOUNTS,
            });
        }
        let mut index = 0i32;
        while self.accounts.iter().any(|a| a.index == index) {
            index += 1;
        }
        let main_dc_id = self
            .active()
            .filter(|a| a.environment == environment)
            .or_else(|| self.accounts.iter().find(|a| a.environment == environment))
            .map_or(0, |a| a.main_dc_id);

        let mut account = Account::new(index, &self.data_name, environment);
        account.main_dc_id = main_dc_id;
        self.storage.start_added(index, environment)?;
        self.accounts.push(account);
        self.persist_accounts()?;
        Ok(index)
    }

    /// Adds and activates a new account if below [`Self::max_accounts`], or activates an existing
    /// unauthenticated account matching `environment` (`Domain::addActivated`).
    pub fn add_activated(&mut self, environment: Environment) -> Result<i32, DomainError> {
        if !self.started() {
            return Err(DomainError::NotStarted);
        }
        if self.accounts.len() < self.max_accounts() {
            let idx = self.add(environment)?;
            self.activate(idx)?;
            return Ok(idx);
        }
        if let Some(existing_idx) = self
            .accounts
            .iter()
            .find(|a| !a.session_exists() && a.environment == environment)
            .map(|a| a.index)
        {
            self.activate(existing_idx)?;
            return Ok(existing_idx);
        }
        Err(DomainError::AccountLimitReached {
            limit: self.max_accounts(),
        })
    }

    /// Switches the active account to `index` (`Domain::activate`).
    ///
    /// If the previously active account had a session, persists the change; if it was
    /// unauthenticated (redundant), removes redundant accounts afterwards.
    pub fn activate(&mut self, index: i32) -> Result<(), DomainError> {
        if !self.started() {
            return Err(DomainError::NotStarted);
        }
        if !self.accounts.iter().any(|a| a.index == index) {
            return Err(DomainError::AccountNotFound(index));
        }
        if self.active_index == Some(index) {
            return Ok(());
        }
        let prev_index = self.active_index;
        let was_authed = prev_index
            .and_then(|idx| self.account(idx))
            .is_some_and(Account::session_exists);

        if prev_index.is_some() {
            self.last_active_index = prev_index;
        }
        self.active_index = Some(index);

        if was_authed {
            self.persist_accounts()?;
        } else {
            let _ = self.remove_redundant_accounts()?;
        }
        Ok(())
    }

    /// Removes the account at `index` (cannot remove the sole remaining account).
    ///
    /// If the active account is removed, activates another account (preferring authenticated ones).
    pub fn remove(&mut self, index: i32) -> Result<Account, DomainError> {
        if !self.started() {
            return Err(DomainError::NotStarted);
        }
        if self.accounts.len() <= 1 {
            return Err(DomainError::CannotRemoveLastAccount);
        }
        let pos = self
            .accounts
            .iter()
            .position(|a| a.index == index)
            .ok_or(DomainError::AccountNotFound(index))?;
        let removed = self.accounts.remove(pos);
        self.storage.account_removed(index)?;

        if self.active_index == Some(index) {
            let next_active = self
                .accounts
                .iter()
                .find(|a| a.session_exists())
                .or_else(|| self.accounts.first())
                .map(|a| a.index);
            self.active_index = next_active;
        }
        if self.last_active_index == Some(index) {
            self.last_active_index = None;
        }

        if !self.remove_passcode_if_empty()? {
            self.persist_accounts()?;
        }
        Ok(removed)
    }

    /// Drops session-less accounts that are not active and have no separate window
    /// (`Domain::removeRedundantAccounts`), returning how many accounts were removed.
    pub fn remove_redundant_accounts(&mut self) -> Result<usize, DomainError> {
        if !self.started() {
            return Err(DomainError::NotStarted);
        }
        if self.accounts.iter().any(|a| a.destroying_session) {
            return Ok(0);
        }
        let active_idx = self.active_index;
        let mut removed_indices = Vec::new();
        self.accounts.retain(|a| {
            let keep = a.session_exists() || a.has_separate_window || Some(a.index) == active_idx;
            if !keep {
                removed_indices.push(a.index);
            }
            keep
        });
        for idx in &removed_indices {
            self.storage.account_removed(*idx)?;
        }
        let removed_count = removed_indices.len();
        if !self.remove_passcode_if_empty()? && removed_count > 0 {
            self.persist_accounts()?;
        }
        Ok(removed_count)
    }

    /// Returns `true` if passcode removal is authorized (`Domain::passcodeRemovalAuthorized`):
    /// single account, no session, and a passcode is currently installed.
    #[must_use]
    pub fn passcode_removal_authorized(&self) -> bool {
        self.accounts.len() == 1
            && self.active().is_some_and(|a| !a.session_exists())
            && self.storage.has_passcode()
    }

    fn remove_passcode_if_empty(&mut self) -> Result<bool, DomainError> {
        if !self.passcode_removal_authorized() {
            return Ok(false);
        }
        self.storage.clear_passcode_after_reset()?;
        self.persist_accounts()?;
        Ok(!self.storage.has_passcode())
    }

    fn persist_accounts(&mut self) -> Result<(), DomainError> {
        let active = self.active_index.unwrap_or(0);
        let indices: Vec<i32> = self.accounts.iter().map(|a| a.index).collect();
        self.storage.write_accounts(active, &indices)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(user_id: i64, unique_id: u64, premium: bool) -> AccountSession {
        AccountSession {
            user_id,
            unique_id,
            premium,
            is_test_mode: false,
            username: format!("user_{user_id}"),
            unread_badge: 2,
            unread_badge_muted: false,
        }
    }

    #[test]
    fn max_accounts_scales_with_premium_from_3_to_6() {
        let storage = MemoryDomainStorage::new("data");
        let mut domain = Domain::new("data", storage);
        assert_eq!(domain.start(b"").unwrap(), StartResult::Success);

        assert_eq!(domain.max_accounts(), 3);

        // Authenticate account 0 as non-premium -> still 3.
        domain
            .account_mut(0)
            .unwrap()
            .create_session(make_session(1, 100, false));
        assert_eq!(domain.max_accounts(), 3);

        // Add 2 more accounts; mark 1 premium -> cap becomes 4.
        let idx1 = domain.add(Environment::Production).unwrap();
        domain
            .account_mut(idx1)
            .unwrap()
            .create_session(make_session(2, 200, true));
        assert_eq!(domain.max_accounts(), 4);

        // Second premium -> cap 5, third premium -> cap 6, fourth premium -> capped at 6.
        let idx2 = domain.add(Environment::Production).unwrap();
        domain
            .account_mut(idx2)
            .unwrap()
            .create_session(make_session(3, 300, true));
        assert_eq!(domain.max_accounts(), 5);

        let idx3 = domain.add(Environment::Production).unwrap();
        domain
            .account_mut(idx3)
            .unwrap()
            .create_session(make_session(4, 400, true));
        assert_eq!(domain.max_accounts(), 6);

        let idx4 = domain.add(Environment::Production).unwrap();
        domain
            .account_mut(idx4)
            .unwrap()
            .create_session(make_session(5, 500, true));
        assert_eq!(domain.max_accounts(), 6);
    }

    #[test]
    fn add_activate_remove_and_ordering() {
        let storage = MemoryDomainStorage::new("data");
        let mut domain = Domain::new("data", storage);
        domain.start(b"").unwrap();

        domain
            .account_mut(0)
            .unwrap()
            .create_session(make_session(10, 1000, false));
        let idx1 = domain.add_activated(Environment::Production).unwrap();
        assert_eq!(idx1, 1);
        assert_eq!(domain.active_index(), Some(1));

        domain
            .account_mut(idx1)
            .unwrap()
            .create_session(make_session(20, 2000, false));
        let idx2 = domain.add(Environment::Production).unwrap();
        assert_eq!(idx2, 2);
        domain
            .account_mut(idx2)
            .unwrap()
            .create_session(make_session(30, 3000, false));

        // 4th account blocked when no premium (max_accounts == 3).
        assert_eq!(
            domain.add_activated(Environment::Production),
            Err(DomainError::AccountLimitReached { limit: 3 })
        );

        // Custom order via accounts_order.
        let ordered = domain.ordered_accounts(&[3000, 1000]);
        let ordered_indices: Vec<i32> = ordered.iter().map(|a| a.index).collect();
        assert_eq!(ordered_indices, vec![2, 0, 1]);

        // Removing active account 1 activates an authenticated sibling and frees slot 1.
        domain.remove(1).unwrap();
        assert_eq!(domain.accounts().len(), 2);
        assert_ne!(domain.active_index(), Some(1));
        assert_eq!(domain.add(Environment::Production).unwrap(), 1);
    }

    #[test]
    fn passcode_cleared_when_last_account_logs_out() {
        let mut storage = MemoryDomainStorage::new("data");
        storage.set_passcode(Some(b"secret".to_vec()));
        let mut acc = Account::new(0, "data", Environment::Production);
        acc.create_session(make_session(1, 10, false));
        storage.seed_account(acc);

        let mut domain = Domain::new("data", storage);
        assert_eq!(
            domain.start(b"wrong").unwrap(),
            StartResult::IncorrectPasscode
        );
        assert_eq!(domain.start(b"secret").unwrap(), StartResult::Success);
        assert!(domain.storage().has_passcode());

        domain.account_mut(0).unwrap().log_out();
        domain.remove_redundant_accounts().unwrap();
        assert!(!domain.storage().has_passcode());
    }
}
