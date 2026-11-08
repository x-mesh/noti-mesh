ALTER TABLE events ADD COLUMN received_at timestamptz;
ALTER TABLE events ALTER COLUMN received_at SET DEFAULT now();
