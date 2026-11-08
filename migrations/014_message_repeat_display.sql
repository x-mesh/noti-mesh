ALTER TABLE gateway_messages ADD COLUMN displayed_count bigint NOT NULL DEFAULT 1;
ALTER TABLE gateway_messages ADD COLUMN refresh_due_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE gateway_messages ADD COLUMN refresh_attempts integer NOT NULL DEFAULT 0;
ALTER TABLE gateway_messages ADD COLUMN refresh_error text;
ALTER TABLE gateway_messages ADD COLUMN refresh_failed_at timestamptz;
