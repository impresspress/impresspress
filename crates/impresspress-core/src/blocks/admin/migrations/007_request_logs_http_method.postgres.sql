-- Name each logged request by the HTTP method its client sent, as rows are
-- written now, instead of the router action older rows stored. The
-- reasoning lives beside the constant that embeds this file, in
-- `migrations/mod.rs`; a shipped migration is hash-addressed over its whole
-- text, so prose here cannot be corrected.

UPDATE impresspress__admin__request_logs SET method = 'GET' WHERE method = 'retrieve';

UPDATE impresspress__admin__request_logs SET method = 'POST' WHERE method = 'create';

UPDATE impresspress__admin__request_logs SET method = 'DELETE' WHERE method = 'delete';

DELETE FROM impresspress__admin__request_logs WHERE method IN ('update', 'execute');
