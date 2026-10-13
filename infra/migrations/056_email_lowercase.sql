-- One spelling per address. The app lowercases every email it receives
-- and looks users up by lower(email), so stored mixed-case addresses are
-- lowercased here. One that would collide with another account's address
-- is left as is (both accounts keep working; merge them by hand).
UPDATE users u SET email = lower(u.email)
WHERE u.email <> lower(u.email)
  AND NOT EXISTS (SELECT 1 FROM users o WHERE o.id <> u.id AND lower(o.email) = lower(u.email));

CREATE INDEX IF NOT EXISTS idx_users_email_lower ON users (lower(email));
