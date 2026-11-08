ALTER TABLE outbox ADD COLUMN target_members jsonb NOT NULL DEFAULT '[]'::jsonb;
ALTER TABLE gateway_messages ADD COLUMN target_members jsonb NOT NULL DEFAULT '[]'::jsonb;
