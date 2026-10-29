-- Savings reports: the amount and currency of a caught duplicate payment,
-- read from the request body when it was deduplicated (reports.rs). Only
-- `deduplicated` rows carry them. The hash chain (024) doesn't cover these
-- columns, so adding them leaves every existing row_hash valid.
ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS money_amount NUMERIC;
ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS money_currency VARCHAR(3);

-- Shadow mode (shadow.rs): an agent's writes are evaluated and recorded
-- (audit_log status 'shadow') but never sent. NULL service/action = every
-- service/action of that agent; the most specific row wins.
CREATE TABLE IF NOT EXISTS shadow_rules (
  id          SERIAL PRIMARY KEY,
  org_id      VARCHAR(255) NOT NULL,
  agent_id    VARCHAR(255) NOT NULL,
  service     VARCHAR(255),
  action      VARCHAR(255),
  block_reads BOOLEAN NOT NULL DEFAULT FALSE,
  stub        JSONB NOT NULL DEFAULT '{}',
  created_by  INTEGER,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CHECK (action IS NULL OR service IS NOT NULL)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_shadow_rules_scope
  ON shadow_rules (org_id, agent_id, COALESCE(service, ''), COALESCE(action, ''));

-- Monthly report email, opt-in per org. last_sent_period ('2026-10') is
-- claimed atomically, so two servers never both send the same month.
CREATE TABLE IF NOT EXISTS report_settings (
  org_id           VARCHAR(255) PRIMARY KEY,
  enabled          BOOLEAN NOT NULL DEFAULT FALSE,
  recipients       TEXT[] NOT NULL DEFAULT '{}',
  last_sent_period VARCHAR(7),
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- White-label colour for Enterprise reports (#rrggbb).
ALTER TABLE org_branding ADD COLUMN IF NOT EXISTS accent_color VARCHAR(7);
