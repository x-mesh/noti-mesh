CREATE TABLE slack_test_settings (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    channel_id text NOT NULL,
    user_id text NOT NULL
);
