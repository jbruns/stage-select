-- Operational settings (#139), edited in the workbench rather than title-select.toml: one row per section, its value
-- JSON. A section with no row has its defaults. Secrets, such as the webhook's URL and signing secret, are kept here in
-- plain text, and the API never returns them.
CREATE TABLE settings (
    section TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
