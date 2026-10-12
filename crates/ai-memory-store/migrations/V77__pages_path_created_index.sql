-- V77: serve the page header's "Created" (the oldest retained version of a
-- path). idx_pages_latest_path is partial on is_latest = 1, so it cannot answer
-- MIN(created_at) over a page's superseded versions; without this index every
-- page view scanned all versions in the project.
CREATE INDEX idx_pages_path_created
    ON pages(workspace_id, project_id, path, created_at);
