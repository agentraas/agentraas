-- 005_redact_audit_api_keys.sql
-- Fixes a real exposure: audit_log.api_key was storing the full raw API key in
-- plaintext, and GET /api/v1/recent returned it directly to the dashboard —
-- meaning any logged-in user could read other agents' live keys from their own
-- activity feed. server.js now masks new entries at the point of insert; this
-- migration redacts anything already stored before that fix landed.
--
-- Keeps a short, non-usable prefix (matches the new masked format) so the audit
-- trail still shows *which* key was used for debugging, without it being usable.

--
-- One-time cleanup: once 024 makes audit_log append-only (trigger
-- audit_log_no_update), this has already run and every later row is masked
-- by the code, so a re-run skips it instead of failing on the trigger.

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = 'audit_log_no_update' AND tgrelid = 'audit_log'::regclass) THEN
    UPDATE audit_log
    SET api_key = CASE
      WHEN api_key IS NULL OR api_key = 'anonymous' OR api_key = 'ak_demo' THEN api_key
      WHEN length(api_key) > 8 THEN substr(api_key, 1, 8) || '…'
      ELSE '••••'
    END
    WHERE api_key IS NOT NULL
      AND api_key NOT LIKE '%…'
      AND api_key NOT IN ('anonymous', 'ak_demo', '••••');
  END IF;
END $$;
