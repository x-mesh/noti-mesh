ALTER TABLE client_token_audit DROP CONSTRAINT client_token_audit_actor_kind_check;
ALTER TABLE client_token_audit ADD CONSTRAINT client_token_audit_actor_kind_check
 CHECK (actor_kind IN ('shared_admin', 'development', 'unknown', 'named_admin'));
ALTER TABLE client_token_audit ADD COLUMN actor text;
ALTER TABLE actions ADD COLUMN initiator text;
ALTER TABLE actions ADD COLUMN initiator_kind text;
