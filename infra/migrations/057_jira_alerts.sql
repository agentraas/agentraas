-- Jira as an alert channel: encrypted_target holds the site, account email
-- and API token (JSON, encrypted); extra holds "PROJECT_KEY|Issue type".
ALTER TABLE notification_webhooks DROP CONSTRAINT IF EXISTS notification_webhooks_type_check;
ALTER TABLE notification_webhooks ADD CONSTRAINT notification_webhooks_type_check
  CHECK (type IN ('slack', 'discord', 'telegram', 'jira'));
