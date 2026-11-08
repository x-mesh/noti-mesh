ALTER TABLE outbox ADD COLUMN mentioned_members jsonb;
ALTER TABLE gateway_messages ADD COLUMN mentioned_members jsonb;
