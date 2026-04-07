-- Enforce a minimum initial balance for JPY paper accounts.
--
-- Derivation: bitFlyer Crypto CFD enforces a 0.001 BTC minimum order size.
-- At a representative BTC price of ~11M JPY with 2× leverage, the required
-- margin for one minimum-size order is roughly 5,500 JPY. Combined with a
-- 10% loss budget per trade and a safety buffer for adverse price moves,
-- 10,000 JPY is the smallest balance that can practically execute and
-- survive a couple of trades. Anything below this would be rejected by the
-- exchange or wiped out instantly.
--
-- Mirrors `paper_accounts::MIN_INITIAL_BALANCE_JPY` and
-- `paper_accounts::normalize_currency` in the application layer. The
-- canonicalization function below uses an explicit ASCII whitespace set
-- (` `, `\t`, `\n`, `\r`) because PostgreSQL's default `BTRIM(text)` only
-- strips spaces — tab/newline would survive a default-trim and silently
-- bypass the CHECK constraint otherwise.
--
-- Note on full-width characters: full-width digits or letters such as
-- `ＪＰＹ` are NOT mapped to canonical `JPY`. They are treated as a
-- distinct (non-JPY) currency token and pass the CHECK by virtue of not
-- being JPY. This matches the application's `to_ascii_uppercase()` behavior
-- and is intentional — non-ASCII currency codes are out of scope for now.

-- Step 1: normalize currency casing AND surrounding ASCII whitespace
-- (space, tab, newline, CR) so the CHECK can rely on canonical form.
UPDATE paper_accounts
   SET currency = UPPER(BTRIM(currency, E' \t\n\r')),
       updated_at = NOW()
 WHERE currency <> UPPER(BTRIM(currency, E' \t\n\r'));

-- Step 2: bring any existing too-small JPY accounts up to the minimum so the
-- new constraint can be added without rejecting legitimate historical data.
-- IMPORTANT: only initial_balance is bumped — current_balance reflects the
-- account's true running P&L and must NOT be inflated by a schema migration.
-- Operators with sub-minimum live balances need to top up out-of-band.
UPDATE paper_accounts
   SET initial_balance = 10000,
       updated_at = NOW()
 WHERE UPPER(BTRIM(currency, E' \t\n\r')) = 'JPY' AND initial_balance < 10000;

-- Step 3: add the CHECK constraint. Defends against raw-SQL inserts that
-- bypass the application's normalize_currency by accepting any casing or
-- ASCII whitespace via UPPER(BTRIM(...)).
-- Drop-then-add for manual re-run idempotency (Postgres ALTER TABLE
-- ADD CONSTRAINT has no IF NOT EXISTS variant).
ALTER TABLE paper_accounts
    DROP CONSTRAINT IF EXISTS paper_accounts_min_jpy_initial_balance;
ALTER TABLE paper_accounts
    ADD CONSTRAINT paper_accounts_min_jpy_initial_balance
    CHECK (UPPER(BTRIM(currency, E' \t\n\r')) <> 'JPY' OR initial_balance >= 10000);
