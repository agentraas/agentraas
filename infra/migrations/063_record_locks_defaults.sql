-- Record locks on by default (record_locks.rs): no row in
-- record_lock_settings now means on, so new rows default to on as well.
-- Custom actions can name the record they change, like catalog actions:
-- {"record": "order", "from": ["order_id"]}.
ALTER TABLE record_lock_settings ALTER COLUMN enabled SET DEFAULT TRUE;
ALTER TABLE custom_actions ADD COLUMN IF NOT EXISTS lock JSONB;
