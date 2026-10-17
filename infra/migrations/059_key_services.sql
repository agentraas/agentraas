-- Per-key service switches: NULL means the key may call every service the
-- org has connected (how every existing key works); a list limits it to
-- those services (built-in names or custom action names).
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS allowed_services TEXT[];
