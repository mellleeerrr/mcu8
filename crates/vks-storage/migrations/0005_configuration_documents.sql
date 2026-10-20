-- Durable, versioned application definitions. No active dialogs or transport resources.
CREATE TABLE IF NOT EXISTS configuration_documents (
    document_key TEXT PRIMARY KEY,
    document_json TEXT NOT NULL
);
