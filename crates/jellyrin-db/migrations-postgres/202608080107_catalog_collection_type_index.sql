-- Measured by qa/postgres-catalog-benchmark.js at 10k/100k/500k rows. The partial
-- collection/name/id index avoids scanning unrelated media domains for stable Movie/TV pages.
CREATE INDEX media_items_visible_collection_name_page_idx
    ON media_items (collection_type, lower(name), id)
    WHERE missing_since IS NULL;
