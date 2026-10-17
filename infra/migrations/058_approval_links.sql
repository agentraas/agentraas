-- Approval links: a held call can be approved from any channel (email,
-- Teams, Telegram, Discord, Slack webhooks) through a one-time link. Only
-- the SHA-256 of the link's token is stored.
ALTER TABLE hitl_requests ADD COLUMN IF NOT EXISTS approval_token_hash TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_hitl_requests_approval_token ON hitl_requests (approval_token_hash) WHERE approval_token_hash IS NOT NULL;

-- Teams and email join the alert channels (they also receive approval links).
ALTER TABLE notification_webhooks DROP CONSTRAINT IF EXISTS notification_webhooks_type_check;
ALTER TABLE notification_webhooks ADD CONSTRAINT notification_webhooks_type_check
  CHECK (type IN ('slack', 'discord', 'telegram', 'jira', 'teams', 'email'));
