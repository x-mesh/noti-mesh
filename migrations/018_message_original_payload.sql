-- Messages converted from another format (for example a Slack incoming webhook) keep the payload the sender posted.
ALTER TABLE gateway_messages ADD COLUMN original_payload jsonb;
