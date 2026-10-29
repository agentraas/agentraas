-- Automatic record locks (record_locks.rs): when on, a call to a catalog
-- action that names a record (a Stripe customer, a HubSpot contact) locks
-- that record for the call, so two agents can't change it at once. Off by
-- default; no row = off.
CREATE TABLE IF NOT EXISTS record_lock_settings (
  org_id     VARCHAR(255) PRIMARY KEY,
  enabled    BOOLEAN NOT NULL DEFAULT FALSE,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
