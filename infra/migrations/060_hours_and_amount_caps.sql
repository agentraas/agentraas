-- Trading hours on action policies: {"tz", "days", "from", "to"}; a call
-- outside them is a violation (block, or approval with on_violation hitl).
ALTER TABLE action_policies ADD COLUMN IF NOT EXISTS hours JSONB;

-- Money caps on spend caps: the sum of a payload field (amount_field) per
-- window may not pass max_amount. A rule may cap calls, money or both.
-- timezone sets where a "day" or "hour" window starts (default UTC).
ALTER TABLE spend_cap_rules ADD COLUMN IF NOT EXISTS amount_field VARCHAR(100);
ALTER TABLE spend_cap_rules ADD COLUMN IF NOT EXISTS max_amount DOUBLE PRECISION CHECK (max_amount > 0);
ALTER TABLE spend_cap_rules ADD COLUMN IF NOT EXISTS timezone VARCHAR(64);
ALTER TABLE spend_cap_rules ALTER COLUMN max_calls DROP NOT NULL;
ALTER TABLE spend_cap_rules DROP CONSTRAINT IF EXISTS spend_cap_rules_limit_check;
ALTER TABLE spend_cap_rules ADD CONSTRAINT spend_cap_rules_limit_check
  CHECK ((max_calls IS NOT NULL OR max_amount IS NOT NULL) AND ((amount_field IS NULL) = (max_amount IS NULL)));
