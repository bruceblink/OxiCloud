-- Folder rollup ETag: introduce `storage.folders.tree_modified_at`,
-- which is bumped whenever any descendant (file or folder) changes.
--
-- Motivation: WebDAV / NextCloud sync clients use a collection's ETag
-- to decide "did anything change inside this folder since I last
-- looked?". Until now `Folder::etag()` returned the folder UUID
-- (constant for the row's life), which made the answer always "no" —
-- forcing clients to do periodic deep PROPFIND walks to discover new
-- files. With this column, `Folder::etag()` becomes
-- `{id_short}-{tree_modified_at}` and clients can do O(changed)
-- recursion instead of O(tree).
--
-- The two triggers cascade an update timestamp up the ltree ancestor
-- chain on every file write and every folder mutation. Performance
-- ceiling: O(depth) row updates per mutation; deep concurrent writes
-- to the same root subtree can contend on the root row.

ALTER TABLE storage.folders
    ADD COLUMN tree_modified_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Backfill existing rows: collapse the rollup timestamp to the
-- per-folder updated_at. Clients re-walking after deploy will see
-- one batch of "looks new to me" responses, which they handle as a
-- content-match-no-download — the expected one-time resync wave.
UPDATE storage.folders SET tree_modified_at = updated_at;


-- File-side trigger: any INSERT/UPDATE/DELETE on storage.files
-- bumps the file's parent folder + all its ancestors in the ltree.
-- Root-level files (folder_id IS NULL) have no ancestors and do not
-- trigger any folder bump — the root listing isn't an etag-emitting
-- collection in OxiCloud's model (no virtual root folder row).
CREATE OR REPLACE FUNCTION storage.bump_folder_tree_from_file()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    target_folder_id UUID;
    target_lpath ltree;
BEGIN
    target_folder_id := COALESCE(NEW.folder_id, OLD.folder_id);
    IF target_folder_id IS NULL THEN
        RETURN COALESCE(NEW, OLD);
    END IF;

    SELECT lpath INTO target_lpath
      FROM storage.folders
     WHERE id = target_folder_id;

    IF target_lpath IS NULL THEN
        RETURN COALESCE(NEW, OLD);
    END IF;

    -- `lpath @> target_lpath` matches the target folder AND every
    -- ancestor up to the root. The GiST index on lpath keeps this
    -- to an index range scan even on deep trees.
    UPDATE storage.folders
       SET tree_modified_at = NOW()
     WHERE lpath @> target_lpath;

    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE TRIGGER files_bump_folder_tree_etag
    AFTER INSERT OR UPDATE OR DELETE ON storage.files
    FOR EACH ROW EXECUTE FUNCTION storage.bump_folder_tree_from_file();


-- Folder-side trigger: covers creates, deletes, renames, and moves.
-- A folder move changes its lpath — the OLD chain and NEW chain
-- both need bumping (old parents lost a child, new parents gained
-- one). Self-exclusion (id <> the changed row) avoids the row
-- bumping itself, which is meaningless and would amplify
-- contention on hot paths.
--
-- The `pg_trigger_depth() > 1` guard breaks recursion: when this
-- trigger UPDATEs ancestor rows below, those UPDATEs would fire
-- the same trigger again. Without the guard, a single child
-- creation would cascade an unbounded number of upward writes.
CREATE OR REPLACE FUNCTION storage.bump_folder_tree_from_folder()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF pg_trigger_depth() > 1 THEN
        RETURN COALESCE(NEW, OLD);
    END IF;

    IF TG_OP IN ('DELETE', 'UPDATE') AND OLD.lpath IS NOT NULL THEN
        UPDATE storage.folders
           SET tree_modified_at = NOW()
         WHERE lpath @> OLD.lpath AND id <> OLD.id;
    END IF;

    IF TG_OP IN ('INSERT', 'UPDATE') AND NEW.lpath IS NOT NULL THEN
        UPDATE storage.folders
           SET tree_modified_at = NOW()
         WHERE lpath @> NEW.lpath AND id <> NEW.id;
    END IF;

    RETURN COALESCE(NEW, OLD);
END;
$$;

CREATE TRIGGER folders_bump_folder_tree_etag
    AFTER INSERT OR UPDATE OR DELETE ON storage.folders
    FOR EACH ROW EXECUTE FUNCTION storage.bump_folder_tree_from_folder();
