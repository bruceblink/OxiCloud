-- Remove rows from auth.user_recent_files and auth.user_favorites whose
-- item_id is not a valid UUID (e.g. composite "uuid1_uuid2" values written
-- by a previous code path that joined owner_id and resource_id with '_').
-- These rows would cause a cast failure on `item_id::UUID` in list queries.

DELETE FROM auth.user_recent_files
WHERE item_id !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$';

DELETE FROM auth.user_favorites
WHERE item_id !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$';
