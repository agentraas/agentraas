-- Jira as an alert channel: encrypted_target holds the site, account email
-- and API token (JSON, encrypted); extra holds "PROJECT_KEY|Issue type".
-- Re-run safe: 058 widens this check later, so only replace a check that
-- doesn't allow 'jira' yet (never narrow 058's list back).
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
    WHERE conname = 'notification_webhooks_type_check'
      AND pg_get_constraintdef(oid) LIKE '%jira%'
  ) THEN
    ALTER TABLE notification_webhooks DROP CONSTRAINT IF EXISTS notification_webhooks_type_check;
    ALTER TABLE notification_webhooks ADD CONSTRAINT notification_webhooks_type_check
      CHECK (type IN ('slack', 'discord', 'telegram', 'jira'));
  END IF;
END $$;
