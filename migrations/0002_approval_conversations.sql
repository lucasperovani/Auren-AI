-- Approvals raised during chat conversations: link the request to the
-- conversation so the chat can show and decide it inline.
ALTER TABLE approval_requests ADD COLUMN conversation_id TEXT REFERENCES conversations(id) ON DELETE CASCADE;
CREATE INDEX idx_approval_requests_conversation_id ON approval_requests(conversation_id, status);