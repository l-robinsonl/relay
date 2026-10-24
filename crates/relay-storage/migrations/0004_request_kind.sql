-- Whether a request is one call or a connection that stays open.
--
-- A column rather than a second table: a websocket is the same
-- document as a request — same URL, headers, authentication,
-- variables — differing only in what happens when you press send.
-- Splitting them would have meant a second case in history, sync,
-- collections and the tree for no gain.
ALTER TABLE requests ADD COLUMN kind TEXT NOT NULL DEFAULT 'http';
