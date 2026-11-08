ALTER TABLE gateway_messages ADD COLUMN displayed_confirmers bigint NOT NULL DEFAULT 0;
-- Existing Slack messages keep their current look until someone confirms them again.
UPDATE gateway_messages m SET displayed_confirmers=(SELECT count(DISTINCT c.actor) FROM message_confirmations c WHERE c.message_id=m.id);
